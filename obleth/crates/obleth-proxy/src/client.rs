//! Who a request came from: its address, the country the edge named, and
//! its User-Agent. Captured once per request by [`capture`], a middleware on
//! the data-plane router, and read back by handlers as a [`ClientInfo`].
//!
//! The address is the rightmost `X-Forwarded-For` entry when the TCP peer is
//! a trusted proxy (`OBLETH_TRUSTED_PROXIES`, default the private and
//! loopback ranges; `none` trusts no proxy), else the peer. The country comes from
//! `OBLETH_CLIENT_COUNTRY_HEADER` (default `cf-ipcountry`; empty turns it
//! off), and only from a trusted proxy, since anyone can send the header.

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

/// The trusted-proxy list and country header, read from the environment once.
pub(crate) struct ClientAddressConfig {
    trusted: Vec<IpNet>,
    country_header: Option<HeaderName>,
}

impl ClientAddressConfig {
    /// Build from the two settings as the environment holds them (`None` =
    /// unset). Bad entries are logged and skipped, never fatal: a typo in the
    /// trusted list must not stop the gateway, and an entry left out only
    /// means its proxy's `X-Forwarded-For` is not believed.
    ///
    /// An unset or blank trusted list means the default private ranges (so a
    /// deployment template can pass the variable through empty); `none`
    /// trusts no proxy at all.
    pub(crate) fn from_settings(trusted: Option<&str>, country_header: Option<&str>) -> Self {
        let trusted = match trusted.map(str::trim) {
            None | Some("") => network::DEFAULT_TRUSTED_PROXIES,
            Some(t) if t.eq_ignore_ascii_case("none") => "",
            Some(t) => t,
        };
        let (trusted, invalid) = network::parse_network_list(trusted);
        if !invalid.is_empty() {
            tracing::warn!(
                entries = ?invalid,
                "OBLETH_TRUSTED_PROXIES has entries that are not CIDR ranges; ignoring them"
            );
        }
        let country_header = match country_header
            .unwrap_or(network::DEFAULT_COUNTRY_HEADER)
            .trim()
        {
            "" => None,
            name => match HeaderName::from_bytes(name.to_ascii_lowercase().as_bytes()) {
                Ok(h) => Some(h),
                Err(_) => {
                    tracing::warn!(
                        header = name,
                        "OBLETH_CLIENT_COUNTRY_HEADER is not a valid header name; not recording countries"
                    );
                    None
                }
            },
        };
        ClientAddressConfig {
            trusted,
            country_header,
        }
    }

    fn from_env() -> Self {
        Self::from_settings(
            std::env::var("OBLETH_TRUSTED_PROXIES").ok().as_deref(),
            std::env::var("OBLETH_CLIENT_COUNTRY_HEADER")
                .ok()
                .as_deref(),
        )
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
        let ip = network::client_ip(
            peer,
            &self.trusted,
            headers
                .get_all("x-forwarded-for")
                .iter()
                .filter_map(|v| v.to_str().ok()),
        );
        let country = match (&self.country_header, from_proxy) {
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
            country,
            user_agent,
            path: path.chars().take(PATH_MAX_CHARS).collect(),
            in_process: false,
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
        let cfg = ClientAddressConfig::from_settings(None, None);
        let h = headers(&[
            ("x-forwarded-for", "6.6.6.6, 203.0.113.9"),
            ("cf-ipcountry", "de"),
            ("user-agent", "curl/8.0"),
        ]);
        let info = cfg.resolve(ip("10.0.3.4"), &h, "/v1/chat/completions");
        assert_eq!(info.ip, ip("203.0.113.9"));
        assert_eq!(info.country, "DE");
        assert_eq!(info.user_agent, "curl/8.0");
        assert_eq!(info.path, "/v1/chat/completions");
        assert_eq!(info.network(), "203.0.113.0/24");
        assert!(!info.in_process);
    }

    #[test]
    fn an_untrusted_peer_cannot_claim_an_address_or_country() {
        let cfg = ClientAddressConfig::from_settings(None, None);
        let h = headers(&[("x-forwarded-for", "203.0.113.9"), ("cf-ipcountry", "DE")]);
        let info = cfg.resolve(ip("198.51.100.20"), &h, "/v1/models");
        assert_eq!(info.ip, ip("198.51.100.20"));
        assert_eq!(info.country, "");
    }

    #[test]
    fn country_is_configurable_and_can_be_turned_off() {
        let h = headers(&[("cf-ipcountry", "DE"), ("x-country", "fr")]);
        let custom = ClientAddressConfig::from_settings(None, Some("X-Country"));
        assert_eq!(custom.resolve(ip("10.0.0.1"), &h, "/").country, "FR");
        let off = ClientAddressConfig::from_settings(None, Some(""));
        assert_eq!(off.resolve(ip("10.0.0.1"), &h, "/").country, "");
        // Placeholders for unknown and Tor are not countries.
        let cfg = ClientAddressConfig::from_settings(None, None);
        for junk in ["XX", "T1", "", "Germany"] {
            let h = headers(&[("cf-ipcountry", junk)]);
            assert_eq!(cfg.resolve(ip("10.0.0.1"), &h, "/").country, "", "{junk}");
        }
    }

    #[test]
    fn the_trusted_list_is_configurable() {
        // Trust only one proxy address; private peers are no longer trusted.
        let cfg = ClientAddressConfig::from_settings(Some("192.0.2.10/32, junk"), None);
        let h = headers(&[("x-forwarded-for", "203.0.113.9")]);
        assert_eq!(cfg.resolve(ip("192.0.2.10"), &h, "/").ip, ip("203.0.113.9"));
        assert_eq!(cfg.resolve(ip("10.0.0.1"), &h, "/").ip, ip("10.0.0.1"));
        // `none` trusts nobody.
        let none = ClientAddressConfig::from_settings(Some("none"), None);
        assert_eq!(none.resolve(ip("10.0.0.1"), &h, "/").ip, ip("10.0.0.1"));
        // Blank is the default, as unset is.
        let blank = ClientAddressConfig::from_settings(Some(" "), None);
        assert_eq!(blank.resolve(ip("10.0.0.1"), &h, "/").ip, ip("203.0.113.9"));
    }

    #[test]
    fn a_request_without_a_peer_has_no_address() {
        let cfg = ClientAddressConfig::from_settings(None, None);
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
        let cfg = ClientAddressConfig::from_settings(None, None);
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
    }

    #[tokio::test]
    async fn the_refusal_is_403_ip_not_allowed_in_openai_shape() {
        let resp = crate::proxy::ip_not_allowed_response(&ClientInfo {
            ip: ip("203.0.113.9"),
            ..Default::default()
        });
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
        let unknown = crate::proxy::ip_not_allowed_response(&ClientInfo::default());
        let bytes = axum::body::to_bytes(unknown.into_body(), 1 << 16)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("an unknown address"));
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
