//! Upstream targets: the ordered endpoints a request is sent to.

use super::*;

/// A model's operator-configured upstream headers as a header map, for every
/// outbound call to that model (the proxied request, helper calls, health
/// probes). Entries were validated when they were written; one that still
/// fails to parse is skipped rather than failing the request.
pub fn upstream_header_map(headers: &obleth_config::UpstreamHeaders) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            out.insert(name, value);
        }
    }
    out
}

/// One resolved upstream target: a base URL plus an optional bearer key, and
/// the model's operator-configured upstream headers.
pub struct UpstreamTarget {
    pub base: String,
    pub api_key: Option<String>,
    pub headers: reqwest::header::HeaderMap,
}

/// Build the ordered list of upstream targets for a request.
///
/// When the model defines explicit endpoints we route across the ones that are
/// both `enabled` and `healthy`. `failover` orders them by ascending priority
/// (lowest first); `load_balance` orders them by a weighted random shuffle so
/// traffic spreads across clusters in proportion to their weights; `session_hash`
/// pins a session to one endpoint via rendezvous hashing of the session key,
/// with the rest following for failover. When no usable endpoints exist we fall
/// back to the model's own `api_base`/`api_key` (or the global default base),
/// preserving the legacy single-upstream path.
pub fn build_targets(
    route: Option<&ResolvedModel>,
    default_base: &str,
    selection_mode: &str,
    session_key: &str,
) -> Vec<UpstreamTarget> {
    let mut targets: Vec<UpstreamTarget> = Vec::new();
    let headers = route
        .map(|r| upstream_header_map(&r.upstream_headers))
        .unwrap_or_default();
    if let Some(r) = route {
        let mut eligible: Vec<&obleth_config::ResolvedEndpoint> = r
            .endpoints
            .iter()
            .filter(|e| e.enabled && e.healthy)
            .collect();
        if !eligible.is_empty() {
            match selection_mode {
                "load_balance" => eligible = weighted_order(eligible),
                "session_hash" => eligible = session_hash_order(eligible, session_key),
                _ => eligible.sort_by_key(|e| e.priority), // failover (default)
            }
            for e in eligible {
                targets.push(UpstreamTarget {
                    base: e.api_base.clone(),
                    api_key: e.api_key.clone().or_else(|| r.api_key.clone()),
                    headers: headers.clone(),
                });
            }
            return targets;
        }
    }
    targets.push(UpstreamTarget {
        base: route
            .map(|r| r.api_base.clone())
            .unwrap_or_else(|| default_base.to_string()),
        api_key: route.and_then(|r| r.api_key.clone()),
        headers,
    });
    targets
}

/// Order endpoints by weighted random sampling (A-Res): each endpoint gets a
/// key `u^(1/weight)` for a uniform random `u`, and we sort by descending key.
/// Higher-weight endpoints land earlier more often, so the first eligible
/// target is chosen in proportion to weight while the rest stay available for
/// failover.
pub fn weighted_order(
    items: Vec<&obleth_config::ResolvedEndpoint>,
) -> Vec<&obleth_config::ResolvedEndpoint> {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut next = move || {
        seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut keyed: Vec<(f64, &obleth_config::ResolvedEndpoint)> = items
        .into_iter()
        .map(|e| {
            let w = e.weight.max(1) as f64;
            let u = (next() as f64 / u64::MAX as f64).max(1e-12);
            (u.powf(1.0 / w), e)
        })
        .collect();
    keyed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    keyed.into_iter().map(|(_, e)| e).collect()
}

/// FNV-1a over `session_key\0endpoint_id` — a small, dependency-free, stable
/// hash. Stability matters: the same key must score an endpoint identically on
/// every request so a session keeps landing on the same replica.
fn rendezvous_score(session_key: &str, endpoint_id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in session_key
        .bytes()
        .chain(std::iter::once(0u8))
        .chain(endpoint_id.bytes())
    {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Order endpoints so a session sticks to one replica: rendezvous (highest-
/// random-weight) hashing scores each endpoint by `hash(session_key, id)`; the
/// highest score is the session's home and goes first, the rest follow for
/// failover. Deterministic for a given key; adding/removing an endpoint
/// reshuffles only minimally. With no session key, falls back to weighted order.
pub fn session_hash_order<'a>(
    items: Vec<&'a obleth_config::ResolvedEndpoint>,
    session_key: &str,
) -> Vec<&'a obleth_config::ResolvedEndpoint> {
    if session_key.is_empty() {
        // No session key on the request, so stickiness is impossible: this falls
        // back to weighted_order, i.e. session_hash behaves like load_balance.
        // Logged (debug, not warn — keyless requests are common and expected) so
        // operators can see why a session_hash model isn't actually sticking.
        tracing::debug!(
            "session_hash selected but request has no session key; falling back to weighted order"
        );
        return weighted_order(items);
    }
    let mut scored: Vec<(u64, &obleth_config::ResolvedEndpoint)> = items
        .into_iter()
        .map(|e| (rendezvous_score(session_key, &e.id), e))
        .collect();
    // Highest score first; tie-break on id for a fully deterministic order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    scored.into_iter().map(|(_, e)| e).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_header_map_holds_the_validated_headers() {
        let headers: obleth_config::UpstreamHeaders =
            [("x-routing-hint".to_string(), "sticky".to_string())].into();
        let map = upstream_header_map(&headers);
        assert_eq!(map.len(), 1);
        assert_eq!(map["x-routing-hint"], "sticky");
    }
}
