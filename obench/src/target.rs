use crate::cli::{Profile, Target};

/// Reject combinations that cannot produce a meaningful result.
pub fn validate_combo(target: Target, profile: Profile) -> Result<(), String> {
    if target == Target::Live && profile == Profile::Extreme {
        return Err(
            "extreme measures the gateway's max req/s and needs the GPU-free demo backend; \
             against live upstreams the generation time dominates. Use --target demo, \
             or pick auto/heavy for live."
                .to_string(),
        );
    }
    Ok(())
}

/// True if a base URL points at the local machine. Used to keep `demo` runs
/// local-only.
fn is_local_url(url: &str) -> bool {
    // authority = the host[:port] between the scheme and the first path segment.
    let authority = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("");
    let host = if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 literal: host is inside the brackets; ignore any :port after ']'.
        rest.split(']').next().unwrap_or(rest)
    } else {
        // host or host:port — drop the port if present.
        authority
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(authority)
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0") || host.ends_with(".localhost")
}

/// Derive the control-plane root URL from an upstream endpoint the user typed.
/// Strips any path (e.g. `/v1`) and keeps `scheme://authority`, so a live
/// endpoint like `https://gateway.example.com/v1` yields watch URLs rooted at
/// `https://gateway.example.com`. If the input has no scheme it is returned trimmed
/// of any path so callers still get a usable base.
pub fn endpoint_root(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split('/').next().unwrap_or(rest);
            format!("{scheme}://{authority}")
        }
        None => url.split('/').next().unwrap_or(url).to_string(),
    }
}

/// The `demo` target seeds synthetic models/tenants/keys into the gateway it is
/// pointed at. That must be the local node — never a remote/shared deployment —
/// so a demo run can't create credentials on someone else's gateway. Live runs
/// are the path for remote endpoints.
pub fn validate_target_locality(
    target: Target,
    admin_base: &str,
    proxy_base: &str,
) -> Result<(), String> {
    if target == Target::Demo && (!is_local_url(admin_base) || !is_local_url(proxy_base)) {
        return Err(format!(
            "demo runs are local-only: they seed demo models/tenants/keys into the \
             gateway and must target this node, not a remote deployment \
             (admin_base={admin_base}, proxy_base={proxy_base}). \
             To benchmark a remote gateway, use --target live."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_extreme_is_rejected_with_reason() {
        let err = validate_combo(Target::Live, Profile::Extreme).unwrap_err();
        assert!(err.contains("demo"));
    }

    #[test]
    fn fixture_extreme_is_allowed() {
        assert!(validate_combo(Target::Demo, Profile::Extreme).is_ok());
    }

    #[test]
    fn live_heavy_is_allowed() {
        assert!(validate_combo(Target::Live, Profile::Heavy).is_ok());
    }

    #[test]
    fn demo_local_is_allowed() {
        assert!(validate_target_locality(
            Target::Demo,
            "http://localhost:9180",
            "http://127.0.0.1:8088"
        )
        .is_ok());
    }

    #[test]
    fn demo_remote_is_rejected() {
        let err = validate_target_locality(
            Target::Demo,
            "https://gateway.example.com",
            "https://gateway.example.com",
        )
        .unwrap_err();
        assert!(err.contains("local-only"));
    }

    #[test]
    fn live_remote_is_allowed() {
        assert!(validate_target_locality(
            Target::Live,
            "https://gateway.example.com",
            "https://gateway.example.com"
        )
        .is_ok());
    }

    #[test]
    fn local_url_variants() {
        assert!(is_local_url("http://localhost:9180"));
        assert!(is_local_url("http://127.0.0.1:8088"));
        assert!(is_local_url("http://[::1]:8088"));
        assert!(is_local_url("http://0.0.0.0:8088"));
        assert!(!is_local_url("https://example.com"));
        assert!(!is_local_url("http://10.0.0.5:8088"));
    }

    #[test]
    fn endpoint_root_strips_path() {
        assert_eq!(
            endpoint_root("https://gateway.example.com/v1"),
            "https://gateway.example.com"
        );
        assert_eq!(
            endpoint_root("https://gateway.example.com"),
            "https://gateway.example.com"
        );
        assert_eq!(
            endpoint_root("http://localhost:8000/v1/"),
            "http://localhost:8000"
        );
        assert_eq!(
            endpoint_root("https://host:443/v1/chat"),
            "https://host:443"
        );
    }
}
