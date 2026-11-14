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

/// How the gateway works out a request's client, as the environment sets
/// it: `OBLETH_TRUSTED_PROXIES`, `OBLETH_CLIENT_ADDRESS_HEADER` and
/// `OBLETH_CLIENT_COUNTRY_HEADER`. Header names are lower-cased.
#[derive(Debug, Clone, Default)]
pub struct ClientAddressSettings {
    pub trusted: Vec<IpNet>,
    /// Entries of the trusted list that are not CIDR ranges, skipped.
    pub invalid: Vec<String>,
    /// A header a trusted proxy sets to exactly the client's address. Unset
    /// by default: the client is found in `X-Forwarded-For`.
    pub address_header: Option<String>,
    /// The header a trusted proxy names the client's country in. `None`
    /// when turned off.
    pub country_header: Option<String>,
}

impl ClientAddressSettings {
    /// From the three settings as the environment holds them (`None` =
    /// unset). An unset or blank trusted list means the default private
    /// ranges (so a deployment template can pass the variable through
    /// empty); `none` trusts no proxy. An unset country header means
    /// [`DEFAULT_COUNTRY_HEADER`]; blank turns it off.
    pub fn from_settings(
        trusted: Option<&str>,
        address_header: Option<&str>,
        country_header: Option<&str>,
    ) -> Self {
        let trusted = match trusted.map(str::trim) {
            None | Some("") => DEFAULT_TRUSTED_PROXIES,
            Some(t) if t.eq_ignore_ascii_case("none") => "",
            Some(t) => t,
        };
        let (trusted, invalid) = parse_network_list(trusted);
        let header = |raw: &str| {
            let raw = raw.trim().to_ascii_lowercase();
            (!raw.is_empty()).then_some(raw)
        };
        ClientAddressSettings {
            trusted,
            invalid,
            address_header: address_header.and_then(header),
            country_header: header(country_header.unwrap_or(DEFAULT_COUNTRY_HEADER)),
        }
    }

    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Self::from_settings(
            var("OBLETH_TRUSTED_PROXIES").as_deref(),
            var("OBLETH_CLIENT_ADDRESS_HEADER").as_deref(),
            var("OBLETH_CLIENT_COUNTRY_HEADER").as_deref(),
        )
    }
}

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

/// The client address in `X-Forwarded-For`, read from the right: each
/// proxy appends the address it saw, so the rightmost entry is the one the
/// nearest proxy vouches for. While that entry is itself a trusted proxy,
/// the entry to its left is the address that proxy saw, so the walk goes on;
/// the first address that is not a trusted proxy is the client. Entries
/// further left came from the client and are never used.
///
/// When every entry is a trusted proxy, the leftmost is the client (a
/// request from inside the proxies' own networks). An entry that is not an
/// address stops the walk: the last address read is used, and when that is
/// the rightmost entry itself, `None` lets the caller fall back to the peer.
pub fn forwarded_client<'a>(
    values: impl IntoIterator<Item = &'a str>,
    trusted: &[IpNet],
) -> Option<IpAddr> {
    let entries: Vec<&str> = values
        .into_iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .collect();
    let mut found = None;
    for entry in entries.iter().rev() {
        let Some(ip) = parse_forwarded_entry(entry).map(canonical_ip) else {
            break;
        };
        found = Some(ip);
        if !trusted.iter().any(|n| n.contains(&ip)) {
            break;
        }
    }
    found
}

/// One address in a header that holds only the client's address (such as
/// `CF-Connecting-IP` or `X-Real-IP`), or `None` when it is not one.
pub fn parse_address_header(raw: &str) -> Option<IpAddr> {
    parse_forwarded_entry(raw).map(canonical_ip)
}

/// The client address for a request. From a trusted proxy: the address in
/// `address_header` when given and it holds one, else the client found in
/// `X-Forwarded-For` (see [`forwarded_client`]), else the peer. From anyone
/// else, the peer itself. `None` only when the peer is unknown (a request
/// built in process).
pub fn client_ip<'a>(
    peer: Option<IpAddr>,
    trusted: &[IpNet],
    address_header: Option<&str>,
    forwarded_for: impl IntoIterator<Item = &'a str>,
) -> Option<IpAddr> {
    let peer = peer.map(canonical_ip);
    match peer {
        Some(p) if trusted.iter().any(|n| n.contains(&p)) => address_header
            .and_then(parse_address_header)
            .or_else(|| forwarded_client(forwarded_for, trusted))
            .or(Some(p)),
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

/// Most named networks one instance keeps.
pub const MAX_NAMED_NETWORKS: usize = 500;

/// Longest name for a network, in characters.
pub const NETWORK_NAME_MAX_CHARS: usize = 64;

/// A range of addresses an operator gave a name, such as a campus, a VPN or
/// a cluster's pod network, so reports can say whose network a client
/// address is in. Purely descriptive: it changes no check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct NamedNetwork {
    /// CIDR range, normalised (`10.139.0.0/16`).
    pub network: String,
    /// What to call it, for example `Campus VPN`.
    pub name: String,
}

/// Validate and normalise named networks as an operator sent them: blank
/// rows are dropped, each range becomes its canonical CIDR, names are
/// trimmed, and a range listed twice is refused. The error names the row
/// that is wrong.
pub fn normalize_named_networks(entries: &[NamedNetwork]) -> Result<Vec<NamedNetwork>, String> {
    let mut out: Vec<NamedNetwork> = Vec::new();
    for entry in entries {
        let raw = entry.network.trim();
        let name: String = entry
            .name
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        if raw.is_empty() && name.is_empty() {
            continue;
        }
        let Some(net) = parse_network(raw) else {
            return Err(format!(
                "\"{raw}\" is not an IP address or CIDR range \
                 (examples: 10.0.0.0/8, 203.0.113.0/24, 2001:db8::/48)"
            ));
        };
        if name.is_empty() {
            return Err(format!("{net} needs a name"));
        }
        if name.chars().count() > NETWORK_NAME_MAX_CHARS {
            return Err(format!(
                "the name for {net} is longer than {NETWORK_NAME_MAX_CHARS} characters"
            ));
        }
        let network = net.to_string();
        if out.iter().any(|n| n.network == network) {
            return Err(format!("{network} is listed twice"));
        }
        out.push(NamedNetwork { network, name });
    }
    if out.len() > MAX_NAMED_NETWORKS {
        return Err(format!(
            "at most {MAX_NAMED_NETWORKS} named networks ({} given)",
            out.len()
        ));
    }
    Ok(out)
}

/// Named networks parsed once, for naming many addresses.
#[derive(Debug, Clone, Default)]
pub struct NetworkNames {
    /// Most specific first, so the first match is the narrowest.
    nets: Vec<(IpNet, String)>,
}

impl NetworkNames {
    /// Entries that do not parse (none should, as the admin API normalises
    /// them) are skipped.
    pub fn new(named: &[NamedNetwork]) -> Self {
        let mut nets: Vec<(IpNet, String)> = named
            .iter()
            .filter_map(|n| parse_network(&n.network).map(|net| (net, n.name.clone())))
            .collect();
        nets.sort_by_key(|n| std::cmp::Reverse(n.0.prefix_len()));
        NetworkNames { nets }
    }

    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }

    /// The name of the narrowest named network that holds all of `raw`, an
    /// address or a CIDR range (a usage row's /24 or /48). A named range
    /// narrower than `raw` does not name it, since only part of `raw` is in
    /// it.
    pub fn name_of(&self, raw: &str) -> Option<&str> {
        let target = parse_network(raw)?;
        self.nets
            .iter()
            .find(|(net, _)| net.contains(&target))
            .map(|(_, name)| name.as_str())
    }
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
            client_ip(peer, &trusted, None, ["203.0.113.9"]),
            Some(ip("203.0.113.9"))
        );
        // A proxy that appends: the client-supplied entries on the left are
        // ignored.
        assert_eq!(
            client_ip(peer, &trusted, None, ["1.1.1.1, 8.8.8.8 ,203.0.113.9"]),
            Some(ip("203.0.113.9"))
        );
        // Several header lines read as one list, in order.
        assert_eq!(
            client_ip(peer, &trusted, None, ["1.1.1.1", "198.51.100.4"]),
            Some(ip("198.51.100.4"))
        );
        // Ports, brackets and IPv6.
        assert_eq!(
            client_ip(peer, &trusted, None, ["203.0.113.9:51234"]),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(
            client_ip(peer, &trusted, None, ["[2001:db8::5]:443"]),
            Some(ip("2001:db8::5"))
        );
        assert_eq!(
            client_ip(peer, &trusted, None, ["2001:db8::5"]),
            Some(ip("2001:db8::5"))
        );
        assert_eq!(
            client_ip(peer, &trusted, None, ["::ffff:203.0.113.9"]),
            Some(ip("203.0.113.9"))
        );
    }

    #[test]
    fn junk_or_missing_forwarded_for_falls_back_to_the_peer() {
        let trusted = default_trusted();
        let peer = Some(ip("10.42.0.7"));
        assert_eq!(client_ip(peer, &trusted, None, []), peer);
        assert_eq!(client_ip(peer, &trusted, None, [""]), peer);
        assert_eq!(client_ip(peer, &trusted, None, [" , "]), peer);
        // A junk rightmost entry does not hand the decision to the
        // client-supplied entries to its left.
        assert_eq!(
            client_ip(peer, &trusted, None, ["203.0.113.9, unknown"]),
            peer
        );
        assert_eq!(
            client_ip(peer, &trusted, None, ["203.0.113.9, 999.1.1.1"]),
            peer
        );
    }

    #[test]
    fn forwarded_for_is_ignored_from_an_untrusted_peer() {
        let trusted = default_trusted();
        let peer = Some(ip("198.51.100.77"));
        assert_eq!(client_ip(peer, &trusted, None, ["203.0.113.9"]), peer);
        // An IPv6 peer outside fc00::/7.
        let peer6 = Some(ip("2001:db8::77"));
        assert_eq!(client_ip(peer6, &trusted, None, ["203.0.113.9"]), peer6);
        // A mapped private peer is the private IPv4 address, so trusted.
        assert_eq!(
            client_ip(Some(ip("::ffff:10.0.0.1")), &trusted, None, ["203.0.113.9"]),
            Some(ip("203.0.113.9"))
        );
        // Trusting nobody.
        assert_eq!(
            client_ip(Some(ip("10.0.0.1")), &[], None, ["203.0.113.9"]),
            Some(ip("10.0.0.1"))
        );
        // No peer at all (an in-process request).
        assert_eq!(client_ip(None, &trusted, None, ["203.0.113.9"]), None);
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

    #[test]
    fn the_walk_skips_trusted_proxies_to_the_client() {
        let trusted = default_trusted();
        let peer = Some(ip("10.42.0.7"));
        // An edge proxy set the client, then the next proxy appended the
        // tunnel it came through: the tunnel is a trusted proxy, so the
        // client to its left is the answer.
        assert_eq!(
            client_ip(peer, &trusted, None, ["203.0.113.9, 10.42.1.90"]),
            Some(ip("203.0.113.9"))
        );
        // The walk stops at the first untrusted address; what the client
        // wrote further left is never used.
        assert_eq!(
            client_ip(
                peer,
                &trusted,
                None,
                ["198.51.100.1, 203.0.113.9, 10.42.1.90, 10.0.0.2"]
            ),
            Some(ip("203.0.113.9"))
        );
        // Only private hops: the leftmost is the client.
        assert_eq!(
            client_ip(peer, &trusted, None, ["10.139.4.5, 10.42.1.90"]),
            Some(ip("10.139.4.5"))
        );
        // Junk behind a trusted hop: the last address read.
        assert_eq!(
            client_ip(peer, &trusted, None, ["unknown, 10.42.1.90"]),
            Some(ip("10.42.1.90"))
        );
        // Trusting only the tunnel's network keeps a private client
        // address as the client.
        let narrow = parse_network_list("10.42.0.0/16").0;
        assert_eq!(
            client_ip(peer, &narrow, None, ["10.139.4.5, 10.42.1.90"]),
            Some(ip("10.139.4.5"))
        );
    }

    #[test]
    fn an_address_header_from_a_trusted_proxy_wins() {
        let trusted = default_trusted();
        let peer = Some(ip("10.42.0.7"));
        assert_eq!(
            client_ip(peer, &trusted, Some("203.0.113.9"), ["10.42.1.90"]),
            Some(ip("203.0.113.9"))
        );
        // Junk in the header falls back to X-Forwarded-For.
        assert_eq!(
            client_ip(peer, &trusted, Some("nope"), ["198.51.100.4"]),
            Some(ip("198.51.100.4"))
        );
        // From an untrusted peer the header is ignored.
        let outside = Some(ip("198.51.100.77"));
        assert_eq!(
            client_ip(outside, &trusted, Some("203.0.113.9"), []),
            outside
        );
    }

    #[test]
    fn named_networks_normalise_and_name_the_narrowest_match() {
        let named = normalize_named_networks(&[
            NamedNetwork {
                network: " 10.0.0.0/8 ".into(),
                name: " Campus ".into(),
            },
            NamedNetwork {
                network: "10.139.5.9/16".into(),
                name: "Research computing".into(),
            },
            NamedNetwork {
                network: "".into(),
                name: "".into(),
            },
            NamedNetwork {
                network: "2001:db8::/32".into(),
                name: "Lab v6".into(),
            },
        ])
        .unwrap();
        assert_eq!(
            named[0],
            NamedNetwork {
                network: "10.0.0.0/8".into(),
                name: "Campus".into()
            }
        );
        assert_eq!(named[1].network, "10.139.0.0/16");
        assert_eq!(named.len(), 3);
        let names = NetworkNames::new(&named);
        assert_eq!(names.name_of("10.139.121.0/24"), Some("Research computing"));
        assert_eq!(names.name_of("10.4.58.0/24"), Some("Campus"));
        assert_eq!(names.name_of("10.4.58.7"), Some("Campus"));
        assert_eq!(names.name_of("2001:db8:1::/48"), Some("Lab v6"));
        assert_eq!(names.name_of("203.0.113.0/24"), None);
        assert_eq!(names.name_of(""), None);
        // A named range narrower than the row names none of it.
        let narrow = NetworkNames::new(&[NamedNetwork {
            network: "192.0.2.0/28".into(),
            name: "x".into(),
        }]);
        assert_eq!(narrow.name_of("192.0.2.0/24"), None);
        assert_eq!(narrow.name_of("192.0.2.3"), Some("x"));
    }

    #[test]
    fn bad_named_networks_are_refused_with_the_row_named() {
        let one = |network: &str, name: &str| {
            normalize_named_networks(&[NamedNetwork {
                network: network.into(),
                name: name.into(),
            }])
        };
        assert!(one("nope", "x").unwrap_err().contains("\"nope\""));
        assert!(one("10.0.0.0/8", " ").unwrap_err().contains("needs a name"));
        assert!(one("10.0.0.0/8", &"n".repeat(65))
            .unwrap_err()
            .contains("longer than"));
        let twice = normalize_named_networks(&[
            NamedNetwork {
                network: "10.0.0.0/8".into(),
                name: "a".into(),
            },
            NamedNetwork {
                network: "10.1.0.0/8".into(),
                name: "b".into(),
            },
        ]);
        assert!(twice.unwrap_err().contains("listed twice"));
    }
}
