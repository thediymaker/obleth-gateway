//! Who a request came from: its address, the country the edge named, and
//! its User-Agent. Captured once per request by [`capture`], a middleware on
//! the data-plane router, and read back by handlers as a [`ClientInfo`].
//!
//! When the TCP peer is a trusted proxy (`OBLETH_TRUSTED_PROXIES`, default
//! the private and loopback ranges; `none` trusts no proxy), the address is
//! the one in `OBLETH_CLIENT_ADDRESS_HEADER` if set, else from
//! `X-Forwarded-For`: when the trusted list is set, the first entry read
//! from the right that is not itself a trusted proxy; with the default, the
//! rightmost entry. From anyone else it is the peer. The country comes from
//! `OBLETH_CLIENT_COUNTRY_HEADER` (default `cf-ipcountry`; empty turns it
//! off), and only from a trusted proxy, since anyone can send the header,
//! and only when the client address it resolved to is public: the edge names
//! a country for internet clients alone, so a client on a private path (a
//! pod, the campus network, a VPN) has no country even when a proxy on that
//! path passes the header through.
//!
//! A country is recorded on usage rows whenever it is found, but it admits a
//! request to a key limited to certain countries only when
//! `OBLETH_TRUSTED_PROXIES` was set ([`ClientInfo::country_admits`]). The
//! default list trusts every private range, so any proxy or pod there could
//! pass on a client-sent header; listing the proxies is the operator saying
//! the ones listed remove that header.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::OnceLock;

use axum::body::Body;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::{header, request::Parts, Extensions, HeaderMap, HeaderName, Request};
use axum::middleware::Next;
use axum::response::Response;
use ipnet::IpNet;
use obleth_config::network;

/// The trusted-proxy list and the address and country headers, read from
/// the environment once.
pub(crate) struct ClientAddressConfig {
    trusted: Vec<IpNet>,
    /// Read `X-Forwarded-For` past the trusted proxies in it: only when the
    /// trusted list was set, see [`network::client_ip`].
    walk: bool,
    address_header: Option<HeaderName>,
    country_header: Option<HeaderName>,
}

impl ClientAddressConfig {
    /// Build from the settings as the environment holds them (`None` =
    /// unset); see [`network::ClientAddressSettings::from_settings`]. Bad
    /// entries are logged and skipped, never fatal: a typo in the trusted
    /// list must not stop the gateway, and an entry left out only means its
    /// proxy's `X-Forwarded-For` is not believed.
    #[cfg(test)]
    pub(crate) fn from_settings(
        trusted: Option<&str>,
        address_header: Option<&str>,
        country_header: Option<&str>,
    ) -> Self {
        Self::from(network::ClientAddressSettings::from_settings(
            trusted,
            address_header,
            country_header,
        ))
    }

    fn from_env() -> Self {
        Self::from(network::ClientAddressSettings::from_env())
    }

    /// What the request's headers and peer say about its client.
    pub(crate) fn resolve(
        &self,
        peer: Option<IpAddr>,
        headers: &HeaderMap,
        path: &str,
    ) -> ClientInfo {
        let peer = peer.map(network::canonical_ip);
        let from_proxy = peer.is_some_and(|p| self.trusted.iter().any(|n| n.contains(&p)));
        let address = self
            .address_header
            .as_ref()
            .and_then(|h| headers.get(h))
            .and_then(|v| v.to_str().ok());
        let ip = network::client_ip(
            peer,
            &self.trusted,
            self.walk,
            address,
            headers
                .get_all("x-forwarded-for")
                .iter()
                .filter_map(|v| v.to_str().ok()),
        );
        // Only a public client address can carry a country: a client inside
        // a private range reached the gateway without the edge, so a header
        // it holds was not set by the edge and must not be believed.
        let public = ip.is_some_and(network::is_public_ip);
        let country = match (&self.country_header, from_proxy && public) {
            (Some(name), true) => headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(network::parse_country)
                .unwrap_or_default(),
            _ => String::new(),
        };
        let user_agent = headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(network::clean_user_agent)
            .unwrap_or_default();
        ClientInfo {
            ip,
            country_admits: self.walk && !country.is_empty(),
            country,
            user_agent,
            path: path.chars().take(PATH_MAX_CHARS).collect(),
            in_process: false,
        }
    }
}

impl From<network::ClientAddressSettings> for ClientAddressConfig {
    fn from(s: network::ClientAddressSettings) -> Self {
        if !s.invalid.is_empty() {
            tracing::warn!(
                entries = ?s.invalid,
                "OBLETH_TRUSTED_PROXIES has entries that are not CIDR ranges; ignoring them"
            );
        }
        let header = |name: Option<String>, var: &str| {
            let name = name?;
            match HeaderName::from_bytes(name.as_bytes()) {
                Ok(h) => Some(h),
                Err(_) => {
                    tracing::warn!(header = %name, "{var} is not a valid header name; ignoring it");
                    None
                }
            }
        };
        ClientAddressConfig {
            trusted: s.trusted,
            walk: s.explicit,
            address_header: header(s.address_header, "OBLETH_CLIENT_ADDRESS_HEADER"),
            country_header: header(s.country_header, "OBLETH_CLIENT_COUNTRY_HEADER"),
        }
    }
}

/// Longest request path kept on a rejection row.
const PATH_MAX_CHARS: usize = 256;

fn config() -> &'static ClientAddressConfig {
    static CONFIG: OnceLock<ClientAddressConfig> = OnceLock::new();
    CONFIG.get_or_init(ClientAddressConfig::from_env)
}

/// Where one request came from. A request that reached a handler without
/// passing [`capture`] (one built in process, or a test calling a handler
/// directly) has no address: a key limited to certain networks is refused
/// for it, which is the safe answer. Work the gateway runs for a client it
/// already checked (batch items) says so with [`ClientInfo::in_process`].
#[derive(Debug, Clone, Default)]
pub(crate) struct ClientInfo {
    pub(crate) ip: Option<IpAddr>,
    /// Two upper-case letters, or empty.
    pub(crate) country: String,
    /// The country may admit the request to a key limited to certain
    /// countries: it was found and `OBLETH_TRUSTED_PROXIES` was set. With the
    /// default trusted list the country is only recorded.
    pub(crate) country_admits: bool,
    pub(crate) user_agent: String,
    /// The request path as received, without its query string.
    pub(crate) path: String,
    /// Gateway-run work for an owner who was checked when it was submitted
    /// (a batch item). Its key's network limits are not checked again.
    pub(crate) in_process: bool,
}

impl ClientInfo {
    /// For work the gateway runs itself on behalf of an owner it already
    /// checked: no address, and no network check.
    pub(crate) fn in_process() -> Self {
        ClientInfo {
            in_process: true,
            ..ClientInfo::default()
        }
    }

    /// The client the middleware recorded on a request, or an unknown one.
    pub(crate) fn of(extensions: &Extensions) -> Self {
        extensions.get::<ClientInfo>().cloned().unwrap_or_default()
    }

    /// The country a key's country limit may be checked against: empty
    /// unless [`ClientInfo::country_admits`].
    pub(crate) fn admitting_country(&self) -> &str {
        if self.country_admits {
            &self.country
        } else {
            ""
        }
    }

    /// The address as text, or empty when unknown.
    pub(crate) fn ip_string(&self) -> String {
        self.ip.map(|ip| ip.to_string()).unwrap_or_default()
    }

    /// The address's /24 or /48 as CIDR text, or empty when unknown.
    pub(crate) fn network(&self) -> String {
        self.ip.map(network::client_network).unwrap_or_default()
    }
}

/// Middleware: record the request's [`ClientInfo`] in its extensions. The
/// peer comes from axum's `ConnectInfo`, present when the listener is served
/// with `into_make_service_with_connect_info`.
pub(crate) async fn capture(mut req: Request<Body>, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let info = config().resolve(peer, req.headers(), req.uri().path());
    req.extensions_mut().insert(info);
    next.run(req).await
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for ClientInfo {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientInfo::of(&parts.extensions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn a_trusted_proxy_supplies_address_and_country() {
        let cfg = ClientAddressConfig::from_settings(None, None, None);
        let h = headers(&[
            ("x-forwarded-for", "6.6.6.6, 203.0.113.9"),
            ("cf-ipcountry", "de"),
            ("user-agent", "curl/8.0"),
        ]);
        let info = cfg.resolve(ip("10.0.3.4"), &h, "/v1/chat/completions");
        assert_eq!(info.ip, ip("203.0.113.9"));
        assert_eq!(info.country, "DE");
        // Recorded, but the default trusted list does not let it admit.
        assert!(!info.country_admits);
        assert_eq!(info.admitting_country(), "");
        assert_eq!(info.user_agent, "curl/8.0");
        assert_eq!(info.path, "/v1/chat/completions");
        assert_eq!(info.network(), "203.0.113.0/24");
        assert!(!info.in_process);
    }

    #[test]
    fn an_untrusted_peer_cannot_claim_an_address_or_country() {
        let cfg = ClientAddressConfig::from_settings(None, None, None);
        let h = headers(&[("x-forwarded-for", "203.0.113.9"), ("cf-ipcountry", "DE")]);
        let info = cfg.resolve(ip("198.51.100.20"), &h, "/v1/models");
        assert_eq!(info.ip, ip("198.51.100.20"));
        assert_eq!(info.country, "");
    }

    #[test]
    fn country_is_configurable_and_can_be_turned_off() {
        let h = headers(&[
            ("x-forwarded-for", "203.0.113.9"),
            ("cf-ipcountry", "DE"),
            ("x-country", "fr"),
        ]);
        let custom = ClientAddressConfig::from_settings(None, None, Some("X-Country"));
        assert_eq!(custom.resolve(ip("10.0.0.1"), &h, "/").country, "FR");
        let off = ClientAddressConfig::from_settings(None, None, Some(""));
        assert_eq!(off.resolve(ip("10.0.0.1"), &h, "/").country, "");
        // Placeholders for unknown and Tor are not countries.
        let cfg = ClientAddressConfig::from_settings(None, None, None);
        for junk in ["XX", "T1", "", "Germany"] {
            let h = headers(&[("x-forwarded-for", "203.0.113.9"), ("cf-ipcountry", junk)]);
            assert_eq!(cfg.resolve(ip("10.0.0.1"), &h, "/").country, "", "{junk}");
        }
    }

    #[test]
    fn only_a_public_client_address_keeps_its_country() {
        let cfg = ClientAddressConfig::from_settings(None, None, None);
        let country_from = |client: &str| {
            let h = headers(&[("x-forwarded-for", client), ("cf-ipcountry", "US")]);
            cfg.resolve(ip("10.0.3.4"), &h, "/").country
        };
        for public in ["203.0.113.9", "8.8.8.8", "2606:4700::1", "::ffff:8.8.8.8"] {
            assert_eq!(country_from(public), "US", "{public}");
        }
        for private in [
            "10.1.2.3",
            "192.168.1.5",
            "172.20.0.9",
            "127.0.0.1",
            "169.254.0.7",
            "100.64.3.2",
            "fd00::5",
            "fe80::1",
            "::1",
            "0.0.0.0",
        ] {
            assert_eq!(country_from(private), "", "{private}");
        }
        // A trusted proxy with no forwarded address: the client is the
        // private proxy itself, so it has no country.
        let h = headers(&[("cf-ipcountry", "US")]);
        assert_eq!(cfg.resolve(ip("10.0.3.4"), &h, "/").country, "");
        // An unknown client has no country either.
        assert_eq!(cfg.resolve(None, &h, "/").country, "");
    }

    #[test]
    fn a_country_admits_only_when_the_proxies_are_listed() {
        let h = headers(&[("x-forwarded-for", "203.0.113.9"), ("cf-ipcountry", "US")]);
        let listed = ClientAddressConfig::from_settings(Some("10.0.0.0/8"), None, None);
        let info = listed.resolve(ip("10.0.3.4"), &h, "/");
        assert_eq!((info.country.as_str(), info.country_admits), ("US", true));
        assert_eq!(info.admitting_country(), "US");
        // A private client never has a country, listed proxies or not.
        let h = headers(&[("x-forwarded-for", "10.1.2.3"), ("cf-ipcountry", "US")]);
        let info = listed.resolve(ip("10.0.3.4"), &h, "/");
        assert_eq!((info.country.as_str(), info.country_admits), ("", false));
    }

    #[test]
    fn the_trusted_list_is_configurable() {
        // Trust only one proxy address; private peers are no longer trusted.
        let cfg = ClientAddressConfig::from_settings(Some("192.0.2.10/32, junk"), None, None);
        let h = headers(&[("x-forwarded-for", "203.0.113.9")]);
        assert_eq!(cfg.resolve(ip("192.0.2.10"), &h, "/").ip, ip("203.0.113.9"));
        assert_eq!(cfg.resolve(ip("10.0.0.1"), &h, "/").ip, ip("10.0.0.1"));
        // `none` trusts nobody.
        let none = ClientAddressConfig::from_settings(Some("none"), None, None);
        assert_eq!(none.resolve(ip("10.0.0.1"), &h, "/").ip, ip("10.0.0.1"));
        // Blank is the default, as unset is.
        let blank = ClientAddressConfig::from_settings(Some(" "), None, None);
        assert_eq!(blank.resolve(ip("10.0.0.1"), &h, "/").ip, ip("203.0.113.9"));
    }

    #[test]
    fn a_client_address_header_names_the_client_behind_a_tunnel() {
        // A tunnel relays to a proxy that appends the tunnel's address.
        let h = headers(&[
            ("x-forwarded-for", "203.0.113.9, 10.42.1.90"),
            ("cf-connecting-ip", "203.0.113.9"),
        ]);
        // With the proxies listed, the walk passes the tunnel.
        let walk = ClientAddressConfig::from_settings(Some("10.42.0.0/16"), None, None);
        assert_eq!(walk.resolve(ip("10.42.3.4"), &h, "/").ip, ip("203.0.113.9"));
        // With the default list, the rightmost entry stands.
        let default = ClientAddressConfig::from_settings(None, None, None);
        assert_eq!(
            default.resolve(ip("10.42.3.4"), &h, "/").ip,
            ip("10.42.1.90")
        );
        let named = ClientAddressConfig::from_settings(None, Some("CF-Connecting-IP"), None);
        let only_header = headers(&[
            ("x-forwarded-for", "10.42.1.90"),
            ("cf-connecting-ip", "198.51.100.8"),
        ]);
        assert_eq!(
            named.resolve(ip("10.42.3.4"), &only_header, "/").ip,
            ip("198.51.100.8")
        );
        // A bad header name is ignored, not fatal.
        let bad = ClientAddressConfig::from_settings(None, Some("bad header"), None);
        assert_eq!(
            bad.resolve(ip("10.42.3.4"), &only_header, "/").ip,
            ip("10.42.1.90")
        );
    }

    #[test]
    fn a_request_without_a_peer_has_no_address() {
        let cfg = ClientAddressConfig::from_settings(None, None, None);
        let h = headers(&[("x-forwarded-for", "203.0.113.9")]);
        let info = cfg.resolve(None, &h, "/");
        assert_eq!(info.ip, None);
        assert_eq!(info.ip_string(), "");
        assert_eq!(info.network(), "");
        assert!(ClientInfo::in_process().in_process);
        assert!(!ClientInfo::of(&Extensions::new()).in_process);
    }

    #[test]
    fn long_user_agents_are_cut() {
        let cfg = ClientAddressConfig::from_settings(None, None, None);
        let long = "a".repeat(600);
        let h = headers(&[("user-agent", long.as_str())]);
        assert_eq!(cfg.resolve(None, &h, "/").user_agent.len(), 256);
    }

    #[test]
    fn the_network_gate_follows_the_key_and_skips_in_process_work() {
        let mut key = crate::boons::test_support::test_key();
        let at = |s: &str| ClientInfo {
            ip: ip(s),
            ..Default::default()
        };
        // No limit: anything goes, an unknown address included.
        assert!(crate::proxy::client_network_allowed(
            &key,
            &at("198.51.100.7")
        ));
        assert!(crate::proxy::client_network_allowed(
            &key,
            &ClientInfo::default()
        ));

        key.allowed_networks = vec!["192.0.2.0/24".into()];
        assert!(crate::proxy::client_network_allowed(&key, &at("192.0.2.9")));
        assert!(crate::proxy::client_network_allowed(
            &key,
            &at("::ffff:192.0.2.9")
        ));
        assert!(!crate::proxy::client_network_allowed(
            &key,
            &at("198.51.100.7")
        ));
        assert!(!crate::proxy::client_network_allowed(
            &key,
            &ClientInfo::default()
        ));
        // A batch item runs for an owner checked at submission.
        assert!(crate::proxy::client_network_allowed(
            &key,
            &ClientInfo::in_process()
        ));

        // A listed country admits a client outside the networks, an unknown
        // address included; an unknown country does not.
        key.allowed_countries = vec!["US".into()];
        let in_country = |s: Option<&str>, c: &str| ClientInfo {
            ip: s.and_then(ip),
            country: c.into(),
            country_admits: !c.is_empty(),
            ..Default::default()
        };
        assert!(crate::proxy::client_network_allowed(
            &key,
            &in_country(Some("198.51.100.7"), "US")
        ));
        assert!(crate::proxy::client_network_allowed(
            &key,
            &in_country(None, "US")
        ));
        assert!(crate::proxy::client_network_allowed(
            &key,
            &in_country(Some("192.0.2.9"), "")
        ));
        assert!(!crate::proxy::client_network_allowed(
            &key,
            &in_country(Some("198.51.100.7"), "DE")
        ));
        assert!(!crate::proxy::client_network_allowed(
            &key,
            &in_country(Some("198.51.100.7"), "")
        ));
        // A country found behind the default trusted list is only recorded.
        let recorded = ClientInfo {
            country_admits: false,
            ..in_country(Some("198.51.100.7"), "US")
        };
        assert!(!crate::proxy::client_network_allowed(&key, &recorded));
        // Countries alone.
        key.allowed_networks.clear();
        assert!(crate::proxy::client_network_allowed(
            &key,
            &in_country(Some("198.51.100.7"), "US")
        ));
        assert!(!crate::proxy::client_network_allowed(
            &key,
            &in_country(Some("192.0.2.9"), "")
        ));
        assert!(crate::proxy::client_network_allowed(
            &key,
            &ClientInfo::in_process()
        ));
    }

    #[tokio::test]
    async fn the_refusal_is_403_ip_not_allowed_in_openai_shape() {
        let mut key = crate::boons::test_support::test_key();
        key.allowed_networks = vec!["192.0.2.0/24".into()];
        let resp = crate::proxy::ip_not_allowed_response(
            &key,
            &ClientInfo {
                ip: ip("203.0.113.9"),
                ..Default::default()
            },
        );
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "ip_not_allowed");
        assert_eq!(body["error"]["type"], crate::proxy::GATEWAY_ERROR_TYPE);
        assert_eq!(
            body["error"]["message"],
            "This API key can't be used from 203.0.113.9. Its owner limited it to certain networks."
        );
        let unknown = crate::proxy::ip_not_allowed_response(&key, &ClientInfo::default());
        let bytes = axum::body::to_bytes(unknown.into_body(), 1 << 16)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("an unknown address"));

        // A key with countries says so, and names the client's country.
        key.allowed_countries = vec!["US".into()];
        let message = |client: ClientInfo| {
            let resp = crate::proxy::ip_not_allowed_response(&key, &client);
            async move {
                let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
                    .await
                    .unwrap();
                let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(body["error"]["code"], "ip_not_allowed");
                body["error"]["message"].as_str().unwrap().to_string()
            }
        };
        assert_eq!(
            message(ClientInfo {
                ip: ip("203.0.113.9"),
                country: "DE".into(),
                country_admits: true,
                ..Default::default()
            })
            .await,
            "This API key can't be used from 203.0.113.9 in DE. \
             Its owner limited the network or country it can be used from."
        );
        assert_eq!(
            message(ClientInfo {
                ip: ip("203.0.113.9"),
                country: "US".into(),
                ..Default::default()
            })
            .await,
            "This API key can't be used from 203.0.113.9 in US, which this gateway can't confirm. \
             Its owner limited the network or country it can be used from."
        );
        assert_eq!(
            message(ClientInfo {
                ip: ip("10.0.0.9"),
                ..Default::default()
            })
            .await,
            "This API key can't be used from 10.0.0.9 (country unknown). \
             Its owner limited the network or country it can be used from."
        );
    }

    /// The middleware sees the real TCP peer when the router is served with
    /// connect info, as `serve_until_shutdown` does: a loopback peer is a
    /// trusted proxy by default, so its `X-Forwarded-For` is believed.
    #[tokio::test]
    async fn the_middleware_records_the_peer_and_forwarded_address() {
        async fn echo(client: ClientInfo) -> String {
            format!("{}|{}|{}", client.ip_string(), client.country, client.path)
        }
        let app = axum::Router::new()
            .route("/v1/echo", axum::routing::get(echo))
            .layer(axum::middleware::from_fn(capture));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let http = reqwest::Client::new();
        let direct = http
            .get(format!("http://{addr}/v1/echo?x=1"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(direct, "127.0.0.1||/v1/echo");
        let proxied = http
            .get(format!("http://{addr}/v1/echo"))
            .header("x-forwarded-for", "6.6.6.6, 203.0.113.9")
            .header("cf-ipcountry", "nl")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(proxied, "203.0.113.9|NL|/v1/echo");
    }
}
