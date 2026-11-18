//! Client network helpers: which address a request came from, which country
//! the edge says it came from, and whether a key's allowed networks and
//! countries admit it.
//!
//! Everything here is pure so it can be tested without a listener. The proxy
//! reads the trusted-proxy list and the country header name from the
//! environment once and applies these per request.

use std::net::IpAddr;

use ipnet::IpNet;

/// Most networks one key may list in `allowed_networks`.
pub const MAX_ALLOWED_NETWORKS: usize = 64;

/// Most countries one key may list in `allowed_countries`.
pub const MAX_ALLOWED_COUNTRIES: usize = 50;

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
    /// The trusted list was set (not the default). Only then does the
    /// gateway read `X-Forwarded-For` past the proxies in it; see
    /// [`client_ip`].
    pub explicit: bool,
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
        let (list, explicit) = match trusted.map(str::trim) {
            None | Some("") => (DEFAULT_TRUSTED_PROXIES, false),
            Some(t) if t.eq_ignore_ascii_case("none") => ("", true),
            Some(t) => (t, true),
        };
        let (trusted, invalid) = parse_network_list(list);
        let header = |raw: &str| {
            let raw = raw.trim().to_ascii_lowercase();
            (!raw.is_empty()).then_some(raw)
        };
        ClientAddressSettings {
            trusted,
            explicit,
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

/// Validate and normalise a key's `allowed_countries` as an operator sent
/// it: blank entries are dropped, each entry is trimmed and upper-cased and
/// must be two ASCII letters (an ISO 3166-1 alpha-2 code such as `US`), and
/// repeats are removed, keeping the first. `XX`, the code edges send for
/// "unknown", is refused. The error names the entry that is wrong.
pub fn normalize_allowed_countries(entries: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for entry in entries.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
        let Some(code) = parse_country(entry) else {
            return Err(format!(
                "allowed_countries: \"{entry}\" is not a two-letter country code \
                 (examples: US, CA, GB)"
            ));
        };
        if !out.contains(&code) {
            out.push(code);
        }
    }
    if out.len() > MAX_ALLOWED_COUNTRIES {
        return Err(format!(
            "allowed_countries: at most {MAX_ALLOWED_COUNTRIES} countries per key ({} given)",
            out.len()
        ));
    }
    Ok(out)
}

/// Whether a key that lists `networks` and `countries` may be used by a
/// client at `ip` in `country` (two upper-case letters, or empty when the
/// edge named none). Both lists empty allows anyone. Otherwise the client is
/// allowed when its address is inside one of the networks (see
/// [`ip_allowed`]) or its country is listed. An unknown address is inside no
/// network and an unknown country matches no country, so a limited key fails
/// closed; a known country alone is enough, whatever the address.
pub fn client_allowed(
    networks: &[String],
    countries: &[String],
    ip: Option<IpAddr>,
    country: &str,
) -> bool {
    if networks.is_empty() && countries.is_empty() {
        return true;
    }
    if !networks.is_empty() && ip_allowed(networks, ip) {
        return true;
    }
    !country.is_empty() && countries.iter().any(|c| c.eq_ignore_ascii_case(country))
}

/// Whether `ip` is a public internet address: not unspecified, loopback,
/// private (RFC 1918), shared/CGNAT (`100.64.0.0/10`), link-local, unique
/// local (`fc00::/7`), multicast, broadcast or otherwise reserved. An
/// IPv4-mapped IPv6 address is judged as its IPv4 address. Documentation
/// ranges count as public so examples read naturally.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match canonical_ip(ip) {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (18..20).contains(&b))
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || (first & 0xffc0) == 0xfec0)
        }
    }
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
/// `X-Forwarded-For`, else the peer. From anyone else, the peer itself.
/// `None` only when the peer is unknown (a request built in process).
///
/// With `walk`, `X-Forwarded-For` is read from the right past the trusted
/// proxies in it (see [`forwarded_client`]); without, its rightmost entry is
/// the client. Walking is safe only when `trusted` lists the proxies alone:
/// a client inside a trusted range could otherwise put any address in the
/// header and have its own skipped as a proxy. So the gateway walks only
/// when `OBLETH_TRUSTED_PROXIES` is set, never with the default of every
/// private range.
pub fn client_ip<'a>(
    peer: Option<IpAddr>,
    trusted: &[IpNet],
    walk: bool,
    address_header: Option<&str>,
    forwarded_for: impl IntoIterator<Item = &'a str>,
) -> Option<IpAddr> {
    let peer = peer.map(canonical_ip);
    match peer {
        Some(p) if trusted.iter().any(|n| n.contains(&p)) => address_header
            .and_then(parse_address_header)
            .or_else(|| forwarded_client(forwarded_for, if walk { trusted } else { &[] }))
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

/// Most named ranges one instance keeps, across all named networks.
pub const MAX_NAMED_NETWORKS: usize = 500;

/// Most named networks (groups of ranges) one instance keeps.
pub const MAX_NAMED_NETWORK_GROUPS: usize = 200;

/// Longest name for a network, in characters.
pub const NETWORK_NAME_MAX_CHARS: usize = 64;

/// A network an operator gave a name, such as a campus, a VPN or a
/// cluster's pod network: one or more ranges of addresses under one name, so
/// reports can say whose network a client address is in. Purely
/// descriptive: it changes no check.
///
/// Reading also accepts the older one-range shape
/// `{"network": "10.0.0.0/8", "name": "Campus"}`, which becomes a group with
/// that one range. Pieces that share a name are merged by
/// [`normalize_named_networks`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(from = "NamedNetworkInput")]
pub struct NamedNetwork {
    /// What to call it, for example `Campus VPN`.
    pub name: String,
    /// CIDR ranges, normalised (`10.139.0.0/16`), in the order given.
    pub ranges: Vec<String>,
    /// The network belongs to the organisation running this gateway. The
    /// gateway changes no check because of it; it tells operators, and apps
    /// built on the gateway, that use from here is expected.
    #[serde(default)]
    pub ours: bool,
}

/// The shapes a named network is read from: the group, or the older single
/// range row.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum NamedNetworkInput {
    // First: an object with `network` is the older row, never a group.
    Legacy {
        network: String,
        #[serde(default)]
        name: String,
    },
    Group {
        #[serde(default)]
        name: String,
        #[serde(default)]
        ranges: Vec<String>,
        #[serde(default)]
        ours: bool,
    },
}

impl From<NamedNetworkInput> for NamedNetwork {
    fn from(input: NamedNetworkInput) -> Self {
        match input {
            NamedNetworkInput::Group { name, ranges, ours } => NamedNetwork { name, ranges, ours },
            NamedNetworkInput::Legacy { network, name } => NamedNetwork {
                name,
                ranges: vec![network],
                ours: false,
            },
        }
    }
}

/// Validate and normalise named networks as an operator sent them: blank
/// groups are dropped, names are trimmed, each range becomes its canonical
/// CIDR, and groups that share a name (ignoring case) are merged into the
/// first, keeping its spelling. A range may belong to one network only. The
/// error names the network or range that is wrong.
pub fn normalize_named_networks(entries: &[NamedNetwork]) -> Result<Vec<NamedNetwork>, String> {
    build_named_networks(entries, true)
}

/// Named networks as saved, read leniently: anything that would be refused
/// by [`normalize_named_networks`] (a range that does not parse, a range
/// already in an earlier network, a group with no name) is skipped instead,
/// so one bad row never hides the others.
pub fn read_named_networks(entries: &[NamedNetwork]) -> Vec<NamedNetwork> {
    build_named_networks(entries, false).unwrap_or_default()
}

fn build_named_networks(
    entries: &[NamedNetwork],
    strict: bool,
) -> Result<Vec<NamedNetwork>, String> {
    macro_rules! refuse {
        ($($arg:tt)*) => {
            if strict {
                return Err(format!($($arg)*));
            } else {
                continue;
            }
        };
    }
    let mut out: Vec<NamedNetwork> = Vec::new();
    // Canonical range -> index of the network that holds it.
    let mut owner: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut total = 0usize;
    for entry in entries {
        let name: String = entry
            .name
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        let raws: Vec<&str> = entry
            .ranges
            .iter()
            .map(|r| r.trim())
            .filter(|r| !r.is_empty())
            .collect();
        if name.is_empty() && raws.is_empty() {
            continue;
        }
        if name.is_empty() {
            refuse!("{} needs a name", raws[0]);
        }
        if name.chars().count() > NETWORK_NAME_MAX_CHARS {
            refuse!(
                "the name \"{}...\" is longer than {NETWORK_NAME_MAX_CHARS} characters",
                name.chars().take(24).collect::<String>()
            );
        }
        if raws.is_empty() {
            refuse!("{name} needs at least one address or range");
        }
        let key = name.to_lowercase();
        let idx = match out.iter().position(|n| n.name.to_lowercase() == key) {
            Some(i) => i,
            None => {
                if out.len() >= MAX_NAMED_NETWORK_GROUPS {
                    refuse!("at most {MAX_NAMED_NETWORK_GROUPS} named networks");
                }
                out.push(NamedNetwork {
                    name,
                    ranges: Vec::new(),
                    ours: false,
                });
                out.len() - 1
            }
        };
        out[idx].ours |= entry.ours;
        for raw in raws {
            let Some(net) = parse_network(raw) else {
                refuse!(
                    "{}: \"{raw}\" is not an IP address or CIDR range \
                     (examples: 10.0.0.0/8, 203.0.113.0/24, 2001:db8::/48)",
                    out[idx].name
                );
            };
            let range = net.to_string();
            match owner.get(&range) {
                // Listed twice in one network: keep the first.
                Some(&i) if i == idx => continue,
                Some(&i) => refuse!(
                    "{range} is in both {} and {}; a range can belong to one network only",
                    out[i].name,
                    out[idx].name
                ),
                None => {}
            }
            if total >= MAX_NAMED_NETWORKS {
                refuse!("at most {MAX_NAMED_NETWORKS} named ranges in all");
            }
            total += 1;
            owner.insert(range.clone(), idx);
            out[idx].ranges.push(range);
        }
    }
    // Only a leniently read group can end up with every range skipped.
    out.retain(|n| !n.ranges.is_empty());
    Ok(out)
}

/// Named networks parsed once, for naming many addresses.
#[derive(Debug, Clone, Default)]
pub struct NetworkNames {
    /// Most specific first, so the first match is the narrowest.
    nets: Vec<(IpNet, String)>,
}

impl NetworkNames {
    /// Ranges that do not parse (none should, as the admin API normalises
    /// them) are skipped. Ranges from every network are considered
    /// together, so an address takes the name of the narrowest range that
    /// holds it, whichever network that range belongs to.
    pub fn new(named: &[NamedNetwork]) -> Self {
        let mut nets: Vec<(IpNet, String)> = named
            .iter()
            .flat_map(|n| {
                n.ranges
                    .iter()
                    .filter_map(|r| parse_network(r).map(|net| (net, n.name.clone())))
            })
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
            client_ip(peer, &trusted, false, None, ["203.0.113.9"]),
            Some(ip("203.0.113.9"))
        );
        // A proxy that appends: the client-supplied entries on the left are
        // ignored.
        assert_eq!(
            client_ip(
                peer,
                &trusted,
                false,
                None,
                ["1.1.1.1, 8.8.8.8 ,203.0.113.9"]
            ),
            Some(ip("203.0.113.9"))
        );
        // Several header lines read as one list, in order.
        assert_eq!(
            client_ip(peer, &trusted, false, None, ["1.1.1.1", "198.51.100.4"]),
            Some(ip("198.51.100.4"))
        );
        // Ports, brackets and IPv6.
        assert_eq!(
            client_ip(peer, &trusted, false, None, ["203.0.113.9:51234"]),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(
            client_ip(peer, &trusted, false, None, ["[2001:db8::5]:443"]),
            Some(ip("2001:db8::5"))
        );
        assert_eq!(
            client_ip(peer, &trusted, false, None, ["2001:db8::5"]),
            Some(ip("2001:db8::5"))
        );
        assert_eq!(
            client_ip(peer, &trusted, false, None, ["::ffff:203.0.113.9"]),
            Some(ip("203.0.113.9"))
        );
    }

    #[test]
    fn junk_or_missing_forwarded_for_falls_back_to_the_peer() {
        let trusted = default_trusted();
        let peer = Some(ip("10.42.0.7"));
        assert_eq!(client_ip(peer, &trusted, false, None, []), peer);
        assert_eq!(client_ip(peer, &trusted, false, None, [""]), peer);
        assert_eq!(client_ip(peer, &trusted, false, None, [" , "]), peer);
        // A junk rightmost entry does not hand the decision to the
        // client-supplied entries to its left.
        assert_eq!(
            client_ip(peer, &trusted, false, None, ["203.0.113.9, unknown"]),
            peer
        );
        assert_eq!(
            client_ip(peer, &trusted, false, None, ["203.0.113.9, 999.1.1.1"]),
            peer
        );
    }

    #[test]
    fn forwarded_for_is_ignored_from_an_untrusted_peer() {
        let trusted = default_trusted();
        let peer = Some(ip("198.51.100.77"));
        assert_eq!(
            client_ip(peer, &trusted, false, None, ["203.0.113.9"]),
            peer
        );
        // An IPv6 peer outside fc00::/7.
        let peer6 = Some(ip("2001:db8::77"));
        assert_eq!(
            client_ip(peer6, &trusted, false, None, ["203.0.113.9"]),
            peer6
        );
        // A mapped private peer is the private IPv4 address, so trusted.
        assert_eq!(
            client_ip(
                Some(ip("::ffff:10.0.0.1")),
                &trusted,
                false,
                None,
                ["203.0.113.9"]
            ),
            Some(ip("203.0.113.9"))
        );
        // Trusting nobody.
        assert_eq!(
            client_ip(Some(ip("10.0.0.1")), &[], false, None, ["203.0.113.9"]),
            Some(ip("10.0.0.1"))
        );
        // No peer at all (an in-process request).
        assert_eq!(
            client_ip(None, &trusted, false, None, ["203.0.113.9"]),
            None
        );
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
            client_ip(peer, &trusted, true, None, ["203.0.113.9, 10.42.1.90"]),
            Some(ip("203.0.113.9"))
        );
        // The walk stops at the first untrusted address; what the client
        // wrote further left is never used.
        assert_eq!(
            client_ip(
                peer,
                &trusted,
                true,
                None,
                ["198.51.100.1, 203.0.113.9, 10.42.1.90, 10.0.0.2"]
            ),
            Some(ip("203.0.113.9"))
        );
        // Only private hops: the leftmost is the client.
        assert_eq!(
            client_ip(peer, &trusted, true, None, ["10.139.4.5, 10.42.1.90"]),
            Some(ip("10.139.4.5"))
        );
        // Junk behind a trusted hop: the last address read.
        assert_eq!(
            client_ip(peer, &trusted, true, None, ["unknown, 10.42.1.90"]),
            Some(ip("10.42.1.90"))
        );
        // Without walking (the default trusted list), a client on a private
        // network can't hide behind an address it wrote itself: the proxy's
        // entry for it is the answer.
        assert_eq!(
            client_ip(peer, &trusted, false, None, ["203.0.113.5, 10.139.121.5"]),
            Some(ip("10.139.121.5"))
        );
        // Trusting only the tunnel's network keeps a private client
        // address as the client.
        let narrow = parse_network_list("10.42.0.0/16").0;
        assert_eq!(
            client_ip(peer, &narrow, true, None, ["10.139.4.5, 10.42.1.90"]),
            Some(ip("10.139.4.5"))
        );
    }

    #[test]
    fn an_address_header_from_a_trusted_proxy_wins() {
        let trusted = default_trusted();
        let peer = Some(ip("10.42.0.7"));
        assert_eq!(
            client_ip(peer, &trusted, false, Some("203.0.113.9"), ["10.42.1.90"]),
            Some(ip("203.0.113.9"))
        );
        // Junk in the header falls back to X-Forwarded-For.
        assert_eq!(
            client_ip(peer, &trusted, false, Some("nope"), ["198.51.100.4"]),
            Some(ip("198.51.100.4"))
        );
        // From an untrusted peer the header is ignored.
        let outside = Some(ip("198.51.100.77"));
        assert_eq!(
            client_ip(outside, &trusted, false, Some("203.0.113.9"), []),
            outside
        );
    }

    fn group(name: &str, ranges: &[&str], ours: bool) -> NamedNetwork {
        NamedNetwork {
            name: name.into(),
            ranges: ranges.iter().map(|r| r.to_string()).collect(),
            ours,
        }
    }

    #[test]
    fn named_networks_normalise_and_name_the_narrowest_match() {
        let named = normalize_named_networks(&[
            group(" Campus ", &[" 10.0.0.0/8 ", "203.0.113.7"], true),
            group("Research computing", &["10.139.5.9/16"], false),
            group("", &["", " "], false),
            group("Lab v6", &["2001:db8::/32"], false),
        ])
        .unwrap();
        assert_eq!(
            named[0],
            group("Campus", &["10.0.0.0/8", "203.0.113.7/32"], true)
        );
        assert_eq!(named[1].ranges, vec!["10.139.0.0/16"]);
        assert_eq!(named.len(), 3);
        let names = NetworkNames::new(&named);
        assert_eq!(names.name_of("10.139.121.0/24"), Some("Research computing"));
        assert_eq!(names.name_of("10.4.58.0/24"), Some("Campus"));
        assert_eq!(names.name_of("10.4.58.7"), Some("Campus"));
        assert_eq!(names.name_of("203.0.113.7"), Some("Campus"));
        assert_eq!(names.name_of("2001:db8:1::/48"), Some("Lab v6"));
        assert_eq!(names.name_of("203.0.113.0/24"), None);
        assert_eq!(names.name_of(""), None);
        // A named range narrower than the row names none of it.
        let narrow = NetworkNames::new(&[group("x", &["192.0.2.0/28"], false)]);
        assert_eq!(narrow.name_of("192.0.2.0/24"), None);
        assert_eq!(narrow.name_of("192.0.2.3"), Some("x"));
    }

    #[test]
    fn the_narrowest_range_across_networks_names_an_address() {
        let named = normalize_named_networks(&[
            group("ASU campus", &["10.0.0.0/8", "129.219.0.0/16"], true),
            group("Research Computing", &["10.139.0.0/16"], true),
            group(
                "Sol",
                &["10.139.120.0/24", "10.139.121.0/24", "10.139.126.0/24"],
                true,
            ),
        ])
        .unwrap();
        let names = NetworkNames::new(&named);
        assert_eq!(names.name_of("10.139.121.5"), Some("Sol"));
        assert_eq!(names.name_of("10.139.121.0/24"), Some("Sol"));
        assert_eq!(names.name_of("10.139.127.0/24"), Some("Research Computing"));
        assert_eq!(names.name_of("10.4.58.0/24"), Some("ASU campus"));
        assert_eq!(names.name_of("129.219.10.1"), Some("ASU campus"));
    }

    #[test]
    fn legacy_rows_merge_by_name() {
        let rows: Vec<NamedNetwork> = serde_json::from_value(serde_json::json!([
            { "network": "10.218.105.0/24", "name": "SCAI" },
            { "network": "10.0.0.0/8", "name": "Campus" },
            { "network": "10.218.107.0/24", "name": " scai " },
            { "network": "10.121.120.0/21", "name": "CreateAI Builder" },
            { "network": "10.121.128.0/22", "name": "CreateAI Builder" }
        ]))
        .unwrap();
        let named = normalize_named_networks(&rows).unwrap();
        assert_eq!(
            named,
            vec![
                group("SCAI", &["10.218.105.0/24", "10.218.107.0/24"], false),
                group("Campus", &["10.0.0.0/8"], false),
                group(
                    "CreateAI Builder",
                    &["10.121.120.0/21", "10.121.128.0/22"],
                    false
                ),
            ]
        );
        // Lenient reading merges the same way.
        assert_eq!(read_named_networks(&rows), named);
    }

    #[test]
    fn legacy_rows_and_groups_mix_and_ours_carries_over() {
        let body: Vec<NamedNetwork> = serde_json::from_value(serde_json::json!([
            { "network": "10.139.120.0/24", "name": "Sol" },
            { "name": "sol", "ranges": ["10.139.121.0/24"], "ours": true },
            { "name": "Lab", "ranges": ["198.51.100.0/24"] }
        ]))
        .unwrap();
        let named = normalize_named_networks(&body).unwrap();
        assert_eq!(
            named,
            vec![
                group("Sol", &["10.139.120.0/24", "10.139.121.0/24"], true),
                group("Lab", &["198.51.100.0/24"], false),
            ]
        );
        // Names are unique ignoring case once merged.
        let twice = normalize_named_networks(&[
            group("VPN", &["10.1.0.0/16"], false),
            group("vpn", &["10.2.0.0/16"], false),
        ])
        .unwrap();
        assert_eq!(twice.len(), 1);
        assert_eq!(twice[0].name, "VPN");
        // The same range twice in one network is kept once.
        let repeat =
            normalize_named_networks(&[group("A", &["10.1.0.0/16", "10.1.2.3/16"], false)])
                .unwrap();
        assert_eq!(repeat[0].ranges, vec!["10.1.0.0/16"]);
    }

    #[test]
    fn named_networks_serialise_in_the_group_shape_and_round_trip() {
        let named = vec![group("Campus VPN", &["203.0.113.0/24"], true)];
        let json = serde_json::to_value(&named).unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                { "name": "Campus VPN", "ranges": ["203.0.113.0/24"], "ours": true }
            ])
        );
        let back: Vec<NamedNetwork> = serde_json::from_value(json).unwrap();
        assert_eq!(back, named);
        // `ours` defaults to false when left out.
        let plain: NamedNetwork =
            serde_json::from_value(serde_json::json!({ "name": "x", "ranges": ["192.0.2.0/24"] }))
                .unwrap();
        assert!(!plain.ours);
    }

    #[test]
    fn bad_named_networks_are_refused_with_the_row_named() {
        let one =
            |range: &str, name: &str| normalize_named_networks(&[group(name, &[range], false)]);
        assert!(one("nope", "x").unwrap_err().contains("\"nope\""));
        assert!(one("10.0.0.0/8", " ").unwrap_err().contains("needs a name"));
        assert!(one("10.0.0.0/8", &"n".repeat(65))
            .unwrap_err()
            .contains("longer than"));
        let empty = normalize_named_networks(&[group("Lab", &[" "], false)]);
        assert!(empty.unwrap_err().contains("Lab needs at least one"));
        let twice = normalize_named_networks(&[
            group("a", &["10.0.0.0/8"], false),
            group("b", &["10.1.0.0/8"], false),
        ])
        .unwrap_err();
        assert!(twice.contains("10.0.0.0/8"), "{twice}");
        assert!(twice.contains("both a and b"), "{twice}");
        let many: Vec<NamedNetwork> = (0..=MAX_NAMED_NETWORK_GROUPS)
            .map(|i| {
                group(
                    &format!("n{i}"),
                    &[&format!("10.{}.{}.0/24", i / 256, i % 256)],
                    false,
                )
            })
            .collect();
        assert!(normalize_named_networks(&many)
            .unwrap_err()
            .contains("at most"));
        let ranges: Vec<String> = (0..=MAX_NAMED_NETWORKS)
            .map(|i| format!("10.{}.{}.0/24", i / 256, i % 256))
            .collect();
        let big = NamedNetwork {
            name: "big".into(),
            ranges,
            ours: false,
        };
        assert!(normalize_named_networks(&[big])
            .unwrap_err()
            .contains("at most"));
    }

    #[test]
    fn reading_saved_networks_skips_what_no_longer_validates() {
        let saved = vec![
            group("Campus", &["10.0.0.0/8", "nope"], false),
            group("", &["192.0.2.0/24"], false),
            group("Pods", &["10.0.0.0/8", "10.42.0.0/16"], true),
        ];
        assert_eq!(
            read_named_networks(&saved),
            vec![
                group("Campus", &["10.0.0.0/8"], false),
                group("Pods", &["10.42.0.0/16"], true),
            ]
        );
    }

    #[test]
    fn countries_normalise_to_unique_upper_case_codes() {
        let got = normalize_allowed_countries(&[
            " us ".into(),
            "Ca".into(),
            "".into(),
            "US".into(),
            "gb".into(),
        ])
        .unwrap();
        assert_eq!(got, ["US", "CA", "GB"]);
        assert_eq!(
            normalize_allowed_countries(&[]).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn bad_countries_are_refused_with_the_entry_named() {
        for bad in ["XX", "xx", "USA", "U", "U1", "12", "Ü S", "united states"] {
            let err = normalize_allowed_countries(&[bad.into()]).unwrap_err();
            assert!(err.contains(&format!("\"{bad}\"")), "{bad}: {err}");
        }
        // Fifty is the most; repeats count once.
        let codes: Vec<String> = (0..26u8)
            .flat_map(|a| {
                (0..26u8).map(move |b| format!("{}{}", (b'A' + a) as char, (b'A' + b) as char))
            })
            .filter(|c| c != "XX")
            .collect();
        assert_eq!(normalize_allowed_countries(&codes[..50]).unwrap().len(), 50);
        let err = normalize_allowed_countries(&codes[..51]).unwrap_err();
        assert!(err.contains("at most 50"), "{err}");
        let repeated = vec!["us".to_string(); 80];
        assert_eq!(normalize_allowed_countries(&repeated).unwrap(), ["US"]);
    }

    #[test]
    fn the_allow_rule_takes_a_network_or_a_country() {
        let none: Vec<String> = Vec::new();
        let nets = vec!["192.0.2.0/24".to_string()];
        let us = vec!["US".to_string()];
        let inside = Some(ip("192.0.2.9"));
        let outside = Some(ip("198.51.100.7"));
        // Both empty: anyone, unknown address and country included.
        assert!(client_allowed(&none, &none, None, ""));
        assert!(client_allowed(&none, &none, outside, "DE"));
        // Networks only.
        assert!(client_allowed(&nets, &none, inside, ""));
        assert!(!client_allowed(&nets, &none, outside, "US"));
        assert!(!client_allowed(&nets, &none, None, "US"));
        // Countries only.
        assert!(client_allowed(&none, &us, outside, "US"));
        assert!(!client_allowed(&none, &us, outside, "DE"));
        assert!(!client_allowed(&none, &us, outside, ""));
        assert!(!client_allowed(&none, &us, inside, ""));
        // Both: either one matching is enough.
        assert!(client_allowed(&nets, &us, inside, ""));
        assert!(client_allowed(&nets, &us, inside, "DE"));
        assert!(client_allowed(&nets, &us, outside, "US"));
        assert!(!client_allowed(&nets, &us, outside, "DE"));
        // An unknown country never matches.
        assert!(!client_allowed(&nets, &us, outside, ""));
        // An unknown address with a listed country is allowed.
        assert!(client_allowed(&nets, &us, None, "US"));
        assert!(client_allowed(&none, &us, None, "US"));
        assert!(!client_allowed(&nets, &us, None, ""));
    }

    #[test]
    fn only_public_addresses_are_public() {
        for public in [
            "8.8.8.8",
            "203.0.113.9",
            "198.51.100.7",
            "100.63.255.255",
            "100.128.0.1",
            "172.32.0.1",
            "2606:4700::1",
            "2001:db8::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public_ip(ip(public)), "{public}");
        }
        for private in [
            "0.0.0.0",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "100.127.255.254",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.0.8",
            "198.18.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "::ffff:10.0.0.1",
            "::ffff:100.64.0.1",
        ] {
            assert!(!is_public_ip(ip(private)), "{private}");
        }
    }
}
