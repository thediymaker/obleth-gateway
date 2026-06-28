//! Read-only upstream failure diagnostics. Invoked only when a model has
//! `debug_diagnostics` on and a terminal 502/504 is about to be returned.
//! DNS resolve + TCP connect only — never ICMP, never mutating, time-boxed.

use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::net::{lookup_host, TcpStream};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Dns,
    ConnectRefused,
    ConnReset,
    Tls,
    UpstreamStatus,
    Timeout,
    Other,
}

#[derive(Debug, Serialize)]
pub struct DnsProbe {
    pub ok: bool,
    pub ips: Vec<String>,
    pub ms: u64,
    pub err: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TcpProbe {
    pub ok: bool,
    pub ms: u64,
    pub err: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UpstreamDiagnostics {
    pub category: ErrorCategory,
    pub host: String,
    pub port: u16,
    pub dns: DnsProbe,
    pub tcp: TcpProbe,
    pub original_error: String,
}

/// Classify a freeform reqwest/hyper error string into a stable category by
/// scanning the lowercased text (same approach as `is_connection_error`).
pub fn classify(last_err: &str) -> ErrorCategory {
    let e = last_err.to_ascii_lowercase();
    let dns = [
        "dns error",
        "failed to lookup address",
        "name or service not known",
        "temporary failure in name resolution",
    ];
    if dns.iter().any(|m| e.contains(m)) {
        return ErrorCategory::Dns;
    }
    if e.contains("connection refused") {
        return ErrorCategory::ConnectRefused;
    }
    if e.contains("connection reset")
        || e.contains("connection aborted")
        || e.contains("broken pipe")
        || e.contains("connection closed")
    {
        return ErrorCategory::ConnReset;
    }
    if e.contains("tls") || e.contains("certificate") || e.contains("handshake") {
        return ErrorCategory::Tls;
    }
    if e.contains("timed out") || e.contains("timeout") {
        return ErrorCategory::Timeout;
    }
    if e.contains("status") {
        return ErrorCategory::UpstreamStatus;
    }
    ErrorCategory::Other
}

/// Probe the upstream behind `url_str`: classify, DNS-resolve the host, then TCP
/// connect to the first address. Never errors out — failures are captured in the
/// probe fields. Bounded by `budget` (split across the two steps).
pub async fn probe_upstream(
    url_str: &str,
    last_err: &str,
    budget: Duration,
) -> UpstreamDiagnostics {
    let category = classify(last_err);
    let (host, port) = match reqwest::Url::parse(url_str) {
        Ok(u) => (
            u.host_str().unwrap_or_default().to_string(),
            u.port_or_known_default().unwrap_or(0),
        ),
        Err(_) => (String::new(), 0),
    };
    let step = budget / 2;

    // DNS step.
    let dns_started = Instant::now();
    let (dns, first_addr) =
        match tokio::time::timeout(step, lookup_host(format!("{host}:{port}"))).await {
            Ok(Ok(iter)) => {
                let addrs: Vec<std::net::SocketAddr> = iter.collect();
                let ips: Vec<String> = addrs.iter().map(|a| a.ip().to_string()).collect();
                let first = addrs.into_iter().next();
                (
                    DnsProbe {
                        ok: first.is_some(),
                        ips,
                        ms: dns_started.elapsed().as_millis() as u64,
                        err: first.is_none().then(|| "resolved 0 addresses".to_string()),
                    },
                    first,
                )
            }
            Ok(Err(e)) => (
                DnsProbe {
                    ok: false,
                    ips: vec![],
                    ms: dns_started.elapsed().as_millis() as u64,
                    err: Some(e.to_string()),
                },
                None,
            ),
            Err(_) => (
                DnsProbe {
                    ok: false,
                    ips: vec![],
                    ms: dns_started.elapsed().as_millis() as u64,
                    err: Some("dns lookup timed out".to_string()),
                },
                None,
            ),
        };

    // TCP step (skipped if DNS produced no address).
    let tcp = match first_addr {
        None => TcpProbe {
            ok: false,
            ms: 0,
            err: Some("skipped: no address".to_string()),
        },
        Some(addr) => {
            let started = Instant::now();
            match tokio::time::timeout(step, TcpStream::connect(addr)).await {
                Ok(Ok(_)) => TcpProbe {
                    ok: true,
                    ms: started.elapsed().as_millis() as u64,
                    err: None,
                },
                Ok(Err(e)) => TcpProbe {
                    ok: false,
                    ms: started.elapsed().as_millis() as u64,
                    err: Some(e.to_string()),
                },
                Err(_) => TcpProbe {
                    ok: false,
                    ms: started.elapsed().as_millis() as u64,
                    err: Some("tcp connect timed out".to_string()),
                },
            }
        }
    };

    UpstreamDiagnostics {
        category,
        host,
        port,
        dns,
        tcp,
        original_error: last_err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn classify_dns_errors() {
        assert!(matches!(
            classify("error trying to connect: dns error: failed to lookup address information"),
            ErrorCategory::Dns
        ));
        assert!(matches!(
            classify("temporary failure in name resolution"),
            ErrorCategory::Dns
        ));
    }

    #[test]
    fn classify_connect_refused() {
        assert!(matches!(
            classify("tcp connect error: connection refused (os error 111)"),
            ErrorCategory::ConnectRefused
        ));
    }

    #[test]
    fn classify_reset_timeout_status_other() {
        assert!(matches!(
            classify("connection reset by peer"),
            ErrorCategory::ConnReset
        ));
        assert!(matches!(
            classify("operation timed out"),
            ErrorCategory::Timeout
        ));
        assert!(matches!(
            classify("upstream returned status 502"),
            ErrorCategory::UpstreamStatus
        ));
        assert!(matches!(classify("something weird"), ErrorCategory::Other));
    }

    #[tokio::test]
    async fn probe_dns_failure_skips_tcp() {
        let d = probe_upstream(
            "http://does-not-exist.invalid:8081/v1/x",
            "dns error",
            Duration::from_secs(2),
        )
        .await;
        assert!(!d.dns.ok);
        assert!(!d.tcp.ok);
        assert_eq!(d.tcp.err.as_deref(), Some("skipped: no address"));
    }

    #[tokio::test]
    async fn probe_tcp_failure_on_closed_port() {
        // 127.0.0.1:1 — resolves fine, nothing listening.
        let d = probe_upstream(
            "http://127.0.0.1:1/v1/x",
            "connection refused",
            Duration::from_secs(2),
        )
        .await;
        assert!(d.dns.ok);
        assert!(!d.tcp.ok);
    }

    #[tokio::test]
    async fn probe_success_against_loopback_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://127.0.0.1:{}/v1/x", addr.port());
        let d = probe_upstream(&url, "whatever", Duration::from_secs(2)).await;
        assert!(d.dns.ok && d.tcp.ok);
    }
}
