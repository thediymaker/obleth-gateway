//! `POST /v1/search` — web search in the Perplexity Search API shape.
//!
//! A search tool is a registry route with `model_type = "search"` whose
//! upstream speaks the SearXNG JSON API (`GET {api_base}/search?q=…&format=json`).
//! The route's name is the tool name clients send, so a search gets the same
//! key auth, model allowlist, tenant input guardrails, fairshare admission and
//! usage ledger as a model call. Request and response follow the Perplexity
//! Search API, which LiteLLM's `/v1/search` also implements, so a client written
//! against either works unchanged:
//!
//! - `POST /v1/search/{tool}`, or `POST /v1/search` naming the tool in
//!   `search_tool_name` (or `model`). With neither, the caller's only search
//!   tool is used.
//! - `GET /v1/search/tools` lists the search tools the caller may use.
//!
//! The unprefixed `/search` spellings are served too, as LiteLLM does.
//!
//! Where LiteLLM passes `max_results` and `search_domain_filter` over in
//! silence, they are honored here: results are cut to `max_results` and kept
//! to (or, for `-domain` entries, kept away from) the listed domains. A
//! search carries no tokens, so it costs nothing against token budgets; the
//! ledger records it with `request_type: "search"`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, Request, Response, StatusCode};
use axum::response::IntoResponse;
use obleth_config::{routing::Candidate, Admission, ResolvedKey, ResolvedModel, SEARCH_MODEL_TYPE};
use obleth_tokenizer::CostEstimate;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::proxy::{
    self, build_targets, effective_admission_weight, error_json, finalize, gate_resolved_key,
    resolve_conversation, resolve_model, surfaced_request_type, RequestMeta,
};
use crate::state::AppState;

/// The canonical path, used for the ledger's request type.
pub const SEARCH_PATH: &str = "/v1/search";

/// The path segment under `/v1/search/` that lists tools rather than naming one.
const TOOLS_SEGMENT: &str = "tools";

/// A search request is a query and a handful of options.
const BODY_MAX: usize = 64 * 1024;

/// The Perplexity spec's bounds.
const MAX_RESULTS_CEILING: u64 = 20;
const MAX_DOMAIN_FILTERS: usize = 20;

/// Long enough for any real query; short enough that a pasted document is
/// refused here rather than by every engine behind the upstream.
const MAX_QUERY_CHARS: usize = 2_000;

/// A search answers in seconds. The route's own timeout still applies when it
/// is shorter, but the long default sized for model generation does not.
const TIMEOUT_CAP: Duration = Duration::from_secs(60);

/// SearXNG options a caller may set directly, beyond the Perplexity fields.
const PASSTHROUGH_PARAMS: &[&str] = &[
    "categories",
    "engines",
    "language",
    "pageno",
    "time_range",
    "safesearch",
];

/// Whether `path` is one of this endpoint's paths: `/v1/search`, `/search`, or
/// either followed by one segment (a tool name, or `tools`). Another API's
/// `…/search` sub-resource is not.
pub(crate) fn is_search_path(path: &str) -> bool {
    let rest = path.strip_prefix("/v1").unwrap_or(path);
    match rest.strip_prefix("/search") {
        Some("") | Some("/") => true,
        Some(tail) => tail
            .strip_prefix('/')
            .is_some_and(|name| !name.is_empty() && !name.contains('/')),
        None => false,
    }
}

/// `POST /v1/search`: the tool comes from the body, or is the only one.
pub async fn search(state: State<AppState>, req: Request<Body>) -> Response<Body> {
    with_request_id(state, None, req).await
}

/// `POST /v1/search/{tool}`.
pub async fn search_named(
    state: State<AppState>,
    Path(tool): Path<String>,
    req: Request<Body>,
) -> Response<Body> {
    with_request_id(state, Some(tool), req).await
}

/// `GET /v1/search/tools`. Registered on the same pattern as
/// [`search_named`], so any other segment is not found.
pub async fn tools(
    State(state): State<AppState>,
    Path(segment): Path<String>,
    req: Request<Body>,
) -> Response<Body> {
    if segment != TOOLS_SEGMENT {
        return error_json(StatusCode::NOT_FOUND, "not found");
    }
    let resolved = match authenticate(&state, req.headers()).await {
        Ok((resolved, _)) => resolved,
        Err(resp) => return resp,
    };
    let data: Vec<Value> = tool_names(&state.model_registry.load(), allowed_for(&resolved))
        .into_iter()
        .map(|name| json!({ "search_tool_name": name, "search_provider": "searxng" }))
        .collect();
    (
        StatusCode::OK,
        axum::Json(json!({ "object": "list", "data": data })),
    )
        .into_response()
}

async fn with_request_id(
    State(state): State<AppState>,
    tool: Option<String>,
    req: Request<Body>,
) -> Response<Body> {
    let request_id = Uuid::new_v4();
    let mut resp = handle(&state, tool, req, request_id).await;
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        resp.headers_mut()
            .entry("x-obleth-request-id")
            .or_insert(value);
    }
    resp
}

/// Bearer auth and the post-auth key gates, as on every endpoint. Returns the
/// key and the device id of an identity credential.
// The error is the finished response the caller returns as is, like
// `gate_resolved_key`'s.
#[allow(clippy::result_large_err)]
async fn authenticate(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<(Arc<ResolvedKey>, String), Response<Body>> {
    let Some(secret) = proxy::bearer(headers) else {
        return Err(error_json(StatusCode::UNAUTHORIZED, "missing bearer token"));
    };
    let cred = crate::jwt_auth::authenticate_credential(state, &secret).await?;
    gate_resolved_key(state, &cred.resolved)?;
    Ok((cred.resolved, cred.device_id))
}

async fn handle(
    state: &AppState,
    tool: Option<String>,
    req: Request<Body>,
    request_id: Uuid,
) -> Response<Body> {
    let started = Instant::now();
    let (parts, body) = req.into_parts();
    let headers = parts.headers;
    let (resolved, device_id) = match authenticate(state, &headers).await {
        Ok(auth) => auth,
        Err(resp) => return resp,
    };

    let body = match axum::body::to_bytes(body, BODY_MAX).await {
        Ok(b) => b,
        Err(_) => return error_json(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    let json: Value = match serde_json::from_slice(&body) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            return error_json(
                StatusCode::BAD_REQUEST,
                "request body must be a JSON object",
            )
        }
    };
    let mut request = match SearchRequest::parse(&json) {
        Ok(r) => r,
        Err(msg) => return error_json(StatusCode::BAD_REQUEST, &msg),
    };
    let route = match pick_tool(state, &resolved, tool.or(request.tool.take())).await {
        Ok(route) => route,
        Err(resp) => return resp,
    };
    let model = route.model_name.clone();

    let conversation = resolve_conversation(
        &headers,
        &json,
        resolved.tenant_id,
        state.session_id_derivation,
    );
    let end_user = proxy::end_user_for(&resolved, &headers, &json);
    let meta = RequestMeta {
        session_id: conversation.value,
        session_id_source: conversation.source.as_str(),
        request_type: surfaced_request_type(&resolved, SEARCH_PATH, &headers),
        device_id,
        end_user: end_user.clone().unwrap_or_default(),
        model_variant: String::new(),
    };

    // ---- tenant input guardrails ----
    // The query leaves for the open web, so the tenant's input policy applies
    // to it as to a prompt. A redaction is searched as redacted.
    let mut scan = json!({ "messages": [{ "role": "user", "content": request.query }] });
    match state
        .boons
        .scan_input(
            state,
            &resolved,
            &meta.session_id,
            request_id,
            &mut scan,
            None,
        )
        .await
    {
        Err(block) => return error_json(block.status, block.reason),
        Ok(true) => match scan["messages"][0]["content"].as_str() {
            Some(query) => request.query = query.to_string(),
            // Fail closed: searching the unredacted query would bypass the
            // policy the scan just applied.
            None => {
                return error_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "guardrails redaction could not be applied",
                )
            }
        },
        Ok(false) => {}
    }

    // ---- fairshare admission ----
    // Zero estimated tokens: a search takes a slot in its tool's pool, which
    // bounds the load on the upstream, but no share of anyone's tokens.
    let est = CostEstimate {
        input_tokens: 0,
        estimated_output_tokens: 0,
    };
    let weight = effective_admission_weight(resolved.weight, Some(&route));
    let admit = state.fairshare.admit(proxy::admit_request_for(
        &resolved,
        &model,
        Some(&route),
        weight,
        0,
        end_user.as_deref(),
    ));
    let admitted = match tokio::time::timeout(proxy::admission_timeout(), admit).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            state.alerts.issue(
                "scheduler_unavailable",
                "Fairshare scheduler unavailable",
                format!(
                    "tenant `{}` model `{model}` path `{SEARCH_PATH}`",
                    resolved.tenant_name
                ),
            );
            return error_json(StatusCode::SERVICE_UNAVAILABLE, "scheduler unavailable");
        }
        Err(_) => {
            let elapsed = started.elapsed().as_millis() as u32;
            finalize(
                state,
                request_id,
                &resolved,
                &meta,
                &model,
                Admission::Rejected,
                est,
                0,
                0,
                0,
                elapsed,
                0,
                elapsed,
                503,
                "off",
                0.0,
                crate::energy::EnergyFigures::default(),
            );
            let mut resp = error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "timed out waiting for search capacity",
            );
            resp.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_static(proxy::ADMISSION_RETRY_AFTER_SECS),
            );
            return resp;
        }
    };
    let queue_wait_ms = admitted.waited.as_millis() as u32;

    let outcome = call_upstream(state, &route, &request, &meta.session_id).await;
    drop(admitted.permit);
    let resp = match outcome {
        Ok(results) => (
            StatusCode::OK,
            axum::Json(json!({ "id": request_id, "object": "search", "results": results })),
        )
            .into_response(),
        Err(failure) => error_json(failure.status, &failure.message),
    };
    finalize(
        state,
        request_id,
        &resolved,
        &meta,
        &model,
        admitted.admission,
        est,
        0,
        0,
        0,
        queue_wait_ms,
        0,
        started.elapsed().as_millis() as u32,
        resp.status().as_u16(),
        "off",
        0.0,
        crate::energy::EnergyFigures::default(),
    );
    resp
}

/// The model allowlist that bounds what a caller may search with, or `None`
/// when it may use everything (internal callers, tenants without a list).
fn allowed_for(resolved: &ResolvedKey) -> Option<&[String]> {
    if resolved.internal {
        None
    } else {
        resolved.allowed_models.as_deref()
    }
}

/// The enabled search tools a caller may use, sorted.
fn tool_names(candidates: &[Candidate], allowed: Option<&[String]>) -> Vec<String> {
    let mut names: Vec<String> = candidates
        .iter()
        .filter(|c| c.model.enabled && c.model.model_type == SEARCH_MODEL_TYPE)
        .filter(|c| allowed.is_none_or(|list| list.iter().any(|m| m == &c.model.model_name)))
        .map(|c| c.model.model_name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Resolve the tool a request names (aliases included), or the caller's only
/// one, and check the caller may use it.
#[allow(clippy::result_large_err)]
async fn pick_tool(
    state: &AppState,
    resolved: &ResolvedKey,
    name: Option<String>,
) -> Result<Arc<ResolvedModel>, Response<Body>> {
    let name = match name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
        Some(name) => name,
        None => {
            let names = tool_names(&state.model_registry.load(), allowed_for(resolved));
            match <[String; 1]>::try_from(names) {
                Ok([only]) => only,
                Err(names) if names.is_empty() => {
                    return Err(error_json(
                        StatusCode::NOT_FOUND,
                        "no search tool is available to this key",
                    ))
                }
                Err(_) => {
                    return Err(error_json(
                        StatusCode::BAD_REQUEST,
                        "search_tool_name is required: GET /v1/search/tools lists the choices",
                    ))
                }
            }
        }
    };
    let Some(route) = resolve_model(state, &name).await else {
        return Err(error_json(
            StatusCode::NOT_FOUND,
            &format!("search tool '{name}' is not registered"),
        ));
    };
    if route.model_type != SEARCH_MODEL_TYPE {
        return Err(error_json(
            StatusCode::BAD_REQUEST,
            &format!(
                "'{}' is registered as '{}', not as a search tool",
                route.model_name, route.model_type
            ),
        ));
    }
    if !route.enabled {
        return Err(error_json(StatusCode::FORBIDDEN, "search tool is disabled"));
    }
    if let Some(allowed) = allowed_for(resolved) {
        if !allowed.iter().any(|m| m == &route.model_name) {
            return Err(error_json(
                StatusCode::FORBIDDEN,
                "model not permitted for tenant",
            ));
        }
    }
    Ok(route)
}

/// Why a search did not produce results, as the client will be told.
#[derive(Debug)]
struct UpstreamFailure {
    status: StatusCode,
    message: String,
}

impl UpstreamFailure {
    fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.into(),
        }
    }
}

/// Run the search against the route's upstreams in the order the route's
/// endpoint selection gives, moving on from one that is unreachable or failing
/// (5xx) and stopping at the first answer.
async fn call_upstream(
    state: &AppState,
    route: &ResolvedModel,
    request: &SearchRequest,
    session: &str,
) -> Result<Vec<Value>, UpstreamFailure> {
    let timeout = route
        .request_timeout_secs
        .and_then(|s| u64::try_from(s).ok())
        .filter(|s| *s > 0)
        .map(Duration::from_secs)
        .unwrap_or(state.upstream_timeout)
        .min(TIMEOUT_CAP);
    let params = request.upstream_params();
    let mut last = UpstreamFailure::bad_gateway("the search tool has no upstream");
    for target in build_targets(
        Some(route),
        &route.api_base,
        &route.endpoint_selection_mode,
        session,
    ) {
        let mut call = state
            .http
            .get(obleth_config::search_url(&target.base))
            .query(&params)
            .headers(target.headers.clone())
            .timeout(timeout);
        if let Some(key) = target.api_key.as_deref().filter(|k| !k.is_empty()) {
            call = call.bearer_auth(key);
        }
        match call.send().await {
            Ok(res) if res.status().is_success() => {
                let body: Value = res.json().await.map_err(|_| {
                    UpstreamFailure::bad_gateway("the search upstream did not answer with JSON")
                })?;
                return Ok(request.shape_results(&body));
            }
            Ok(res) if res.status().is_server_error() => {
                last = UpstreamFailure::bad_gateway(format!(
                    "the search upstream failed (HTTP {})",
                    res.status().as_u16()
                ));
            }
            Ok(res) => return Err(rejection(res.status())),
            Err(error) => {
                tracing::warn!(%error, tool = %route.model_name, "search upstream unreachable");
                last = UpstreamFailure::bad_gateway("the search upstream is unreachable");
            }
        }
    }
    Err(last)
}

/// A 4xx from the upstream, translated: it is the gateway's configuration or
/// the upstream's limits at fault, not the client's request (which was
/// validated here).
fn rejection(status: reqwest::StatusCode) -> UpstreamFailure {
    match status.as_u16() {
        429 => UpstreamFailure {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: "the search upstream is rate limiting; retry shortly".into(),
        },
        // SearXNG answers 403 when its settings do not enable `format=json`.
        403 => UpstreamFailure::bad_gateway(
            "the search upstream refused the request (HTTP 403); its JSON output may be disabled",
        ),
        code => UpstreamFailure::bad_gateway(format!(
            "the search upstream rejected the request (HTTP {code})"
        )),
    }
}

/// `search_domain_filter`: domains to keep results to, and (`-` prefixed)
/// domains to keep them away from. A domain covers its subdomains.
#[derive(Debug, Default, PartialEq)]
struct DomainFilter {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl DomainFilter {
    fn parse(entries: &[&str]) -> Self {
        let mut filter = Self::default();
        for entry in entries {
            let entry = entry.trim();
            let (list, raw) = match entry.strip_prefix('-') {
                Some(rest) => (&mut filter.exclude, rest),
                None => (&mut filter.include, entry),
            };
            let domain = normalize_domain(raw);
            if !domain.is_empty() && !list.contains(&domain) {
                list.push(domain);
            }
        }
        filter
    }

    fn admits(&self, url: &str) -> bool {
        if self.include.is_empty() && self.exclude.is_empty() {
            return true;
        }
        let Some(host) = reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        else {
            return false;
        };
        let covers = |domain: &String| {
            host == *domain
                || host
                    .strip_suffix(domain.as_str())
                    .is_some_and(|sub| sub.ends_with('.'))
        };
        (self.include.is_empty() || self.include.iter().any(covers))
            && !self.exclude.iter().any(covers)
    }
}

/// `https://www.Example.org/path` → `example.org`: the scheme, path and a
/// leading `www.` are spelling, not scope.
fn normalize_domain(raw: &str) -> String {
    let raw = raw.trim().to_ascii_lowercase();
    let raw = raw.split_once("://").map_or(raw.as_str(), |(_, rest)| rest);
    let host = raw.split(['/', '?', '#']).next().unwrap_or("");
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// A SearXNG `language` for a Perplexity `country` (ISO 3166-1 alpha-2). A
/// country names a region, not a language, so only countries with one obvious
/// search language are mapped; for any other, the upstream's default applies.
fn language_for_country(country: &str) -> Option<&'static str> {
    Some(match country.trim().to_ascii_uppercase().as_str() {
        "US" => "en-US",
        "GB" | "UK" => "en-GB",
        "AU" => "en-AU",
        "CA" => "en-CA",
        "NZ" => "en-NZ",
        "IE" => "en-IE",
        "IN" => "en-IN",
        "DE" => "de-DE",
        "AT" => "de-AT",
        "FR" => "fr-FR",
        "ES" => "es-ES",
        "MX" => "es-MX",
        "AR" => "es-AR",
        "IT" => "it-IT",
        "PT" => "pt-PT",
        "BR" => "pt-BR",
        "NL" => "nl-NL",
        "JP" => "ja-JP",
        "KR" => "ko-KR",
        "CN" => "zh-CN",
        "TW" => "zh-TW",
        "PL" => "pl-PL",
        "SE" => "sv-SE",
        _ => return None,
    })
}

/// A validated search request.
#[derive(Debug)]
struct SearchRequest {
    /// The tool named in the body (`search_tool_name`, else `model`).
    tool: Option<String>,
    query: String,
    max_results: Option<usize>,
    domains: DomainFilter,
    /// SearXNG options, in [`PASSTHROUGH_PARAMS`] order.
    params: Vec<(&'static str, String)>,
}

impl SearchRequest {
    fn parse(json: &Value) -> Result<Self, String> {
        let tool = ["search_tool_name", "model"]
            .iter()
            .find_map(|k| json.get(*k).and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // A list of queries is searched as one, joined, as LiteLLM does: a
        // SearXNG search takes a single query.
        let query = match json.get("query") {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::trim))
                .collect::<Option<Vec<&str>>>()
                .ok_or("query must be a string or a list of strings")?
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            Some(Value::Null) | None => return Err("query is required".into()),
            Some(_) => return Err("query must be a string or a list of strings".into()),
        };
        if query.is_empty() {
            return Err("query must not be empty".into());
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(format!(
                "query is too long (maximum {MAX_QUERY_CHARS} characters)"
            ));
        }

        let max_results = match json.get("max_results") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_u64() {
                Some(n @ 1..=MAX_RESULTS_CEILING) => Some(n as usize),
                _ => {
                    return Err(format!(
                        "max_results must be an integer from 1 to {MAX_RESULTS_CEILING}"
                    ))
                }
            },
        };

        let domains = match json.get("search_domain_filter") {
            None | Some(Value::Null) => DomainFilter::default(),
            Some(Value::Array(items)) => {
                if items.len() > MAX_DOMAIN_FILTERS {
                    return Err(format!(
                        "search_domain_filter takes at most {MAX_DOMAIN_FILTERS} domains"
                    ));
                }
                let entries = items
                    .iter()
                    .map(Value::as_str)
                    .collect::<Option<Vec<&str>>>()
                    .ok_or("search_domain_filter must be a list of domains")?;
                DomainFilter::parse(&entries)
            }
            Some(_) => return Err("search_domain_filter must be a list of domains".into()),
        };

        let mut params = Vec::new();
        for key in PASSTHROUGH_PARAMS {
            let value = match json.get(*key) {
                None | Some(Value::Null) => continue,
                Some(Value::String(s)) if s.trim().is_empty() => continue,
                Some(Value::String(s)) => s.trim().to_string(),
                Some(Value::Number(n)) => n.to_string(),
                // `categories` and `engines` read naturally as lists.
                Some(Value::Array(items)) => items
                    .iter()
                    .map(Value::as_str)
                    .collect::<Option<Vec<&str>>>()
                    .ok_or(format!("{key} must be a string"))?
                    .join(","),
                Some(_) => return Err(format!("{key} must be a string")),
            };
            params.push((*key, value));
        }
        if !params.iter().any(|(k, _)| *k == "language") {
            if let Some(language) = json
                .get("country")
                .and_then(Value::as_str)
                .and_then(language_for_country)
            {
                params.push(("language", language.to_string()));
            }
        }

        Ok(Self {
            tool,
            query,
            max_results,
            domains,
            params,
        })
    }

    /// The SearXNG query string. A single kept domain also narrows the search
    /// itself with `site:` (the engines that ignore it are filtered after),
    /// so a narrow domain is not starved by results from everywhere else.
    fn upstream_params(&self) -> Vec<(&'static str, String)> {
        let q = match self.domains.include.as_slice() {
            [domain] => format!("{} site:{domain}", self.query),
            _ => self.query.clone(),
        };
        let mut out = vec![("q", q), ("format", "json".to_string())];
        out.extend(self.params.iter().cloned());
        out
    }

    /// SearXNG results in the Perplexity shape, filtered and cut to size.
    fn shape_results(&self, upstream: &Value) -> Vec<Value> {
        let text =
            |r: &Value, key: &str| r.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        upstream
            .get("results")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|r| Some((r, r.get("url")?.as_str()?)))
            .filter(|(_, url)| self.domains.admits(url))
            .take(self.max_results.unwrap_or(usize::MAX))
            .map(|(r, url)| {
                let date = ["publishedDate", "pubdate"]
                    .iter()
                    .find_map(|k| r.get(*k).filter(|v| !v.is_null()))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({
                    "title": text(r, "title"),
                    "url": url,
                    "snippet": text(r, "content"),
                    "date": date,
                    "last_updated": Value::Null,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: Value) -> SearchRequest {
        SearchRequest::parse(&body).expect("valid request")
    }

    fn refused(body: Value) -> String {
        SearchRequest::parse(&body).expect_err("invalid request")
    }

    #[test]
    fn search_paths_are_this_endpoint_only() {
        for path in [
            "/v1/search",
            "/v1/search/",
            "/v1/search/web",
            "/v1/search/tools",
            "/search",
            "/search/web",
        ] {
            assert!(is_search_path(path), "{path}");
        }
        for path in [
            "/v1/searches",
            "/v1/search/web/extra",
            "/v1/vector_stores/vs_1/search",
            "/v1/chat/completions",
        ] {
            assert!(!is_search_path(path), "{path}");
        }
    }

    #[test]
    fn a_minimal_request_is_a_query_asking_for_json() {
        let r = parse(json!({ "query": "  gaudi 3 memory  " }));
        assert_eq!(r.tool, None);
        assert_eq!(
            r.upstream_params(),
            vec![
                ("q", "gaudi 3 memory".to_string()),
                ("format", "json".to_string())
            ]
        );
    }

    #[test]
    fn the_tool_is_read_from_search_tool_name_then_model() {
        let r = parse(json!({ "query": "x", "search_tool_name": "web", "model": "other" }));
        assert_eq!(r.tool.as_deref(), Some("web"));
        let r = parse(json!({ "query": "x", "model": "web" }));
        assert_eq!(r.tool.as_deref(), Some("web"));
    }

    #[test]
    fn a_list_of_queries_is_searched_as_one() {
        let r = parse(json!({ "query": ["rust", " ", "axum"] }));
        assert_eq!(r.query, "rust axum");
    }

    #[test]
    fn malformed_requests_are_refused_with_the_reason() {
        assert_eq!(refused(json!({})), "query is required");
        assert_eq!(refused(json!({ "query": "  " })), "query must not be empty");
        assert!(refused(json!({ "query": 3 })).contains("string"));
        assert!(refused(json!({ "query": ["a", 1] })).contains("string"));
        assert!(refused(json!({ "query": "x".repeat(MAX_QUERY_CHARS + 1) })).contains("too long"));
        assert!(refused(json!({ "query": "x", "max_results": 0 })).contains("1 to 20"));
        assert!(refused(json!({ "query": "x", "max_results": 21 })).contains("1 to 20"));
        assert!(refused(json!({ "query": "x", "max_results": "5" })).contains("1 to 20"));
        let many: Vec<String> = (0..21).map(|i| format!("d{i}.org")).collect();
        assert!(refused(json!({ "query": "x", "search_domain_filter": many })).contains("at most"));
        assert!(refused(json!({ "query": "x", "search_domain_filter": "a.org" })).contains("list"));
        assert!(refused(json!({ "query": "x", "engines": { "a": 1 } })).contains("engines"));
    }

    #[test]
    fn searxng_options_pass_through_and_country_picks_a_language() {
        let r = parse(json!({
            "query": "x",
            "engines": ["google", "bing"],
            "pageno": 2,
            "time_range": "month",
            "country": "DE",
            "unknown_option": "dropped",
        }));
        assert_eq!(
            r.upstream_params(),
            vec![
                ("q", "x".to_string()),
                ("format", "json".to_string()),
                ("engines", "google,bing".to_string()),
                ("pageno", "2".to_string()),
                ("time_range", "month".to_string()),
                ("language", "de-DE".to_string()),
            ]
        );
        // An explicit language wins over the country; an unmapped country
        // leaves the upstream's default alone.
        let r = parse(json!({ "query": "x", "language": "fr", "country": "US" }));
        assert_eq!(r.params, vec![("language", "fr".to_string())]);
        let r = parse(json!({ "query": "x", "country": "CH" }));
        assert!(r.params.is_empty());
    }

    #[test]
    fn domain_filters_keep_and_exclude_by_domain_and_subdomain() {
        let f = DomainFilter::parse(&[
            "https://www.Nature.com/articles",
            "arxiv.org",
            "-blog.arxiv.org",
        ]);
        assert_eq!(f.include, vec!["nature.com", "arxiv.org"]);
        assert_eq!(f.exclude, vec!["blog.arxiv.org"]);
        assert!(f.admits("https://www.nature.com/x"));
        assert!(f.admits("https://arxiv.org/abs/1"));
        assert!(f.admits("https://export.arxiv.org/abs/1"));
        assert!(!f.admits("https://blog.arxiv.org/post"));
        // A suffix that is not a subdomain boundary does not count.
        assert!(!f.admits("https://notarxiv.org/abs/1"));
        assert!(!f.admits("https://example.com/"));
        assert!(!f.admits("not a url"));

        let only_exclude = DomainFilter::parse(&["-pinterest.com"]);
        assert!(only_exclude.admits("https://example.com/"));
        assert!(!only_exclude.admits("https://www.pinterest.com/pin/1"));
        assert!(DomainFilter::default().admits("not a url"));
    }

    #[test]
    fn one_kept_domain_also_narrows_the_query() {
        let r = parse(json!({ "query": "attention", "search_domain_filter": ["arxiv.org"] }));
        assert_eq!(
            r.upstream_params()[0],
            ("q", "attention site:arxiv.org".to_string())
        );
        let r =
            parse(json!({ "query": "attention", "search_domain_filter": ["arxiv.org", "-x.org"] }));
        assert_eq!(
            r.upstream_params()[0],
            ("q", "attention site:arxiv.org".to_string())
        );
        let r = parse(json!({ "query": "attention", "search_domain_filter": ["a.org", "b.org"] }));
        assert_eq!(r.upstream_params()[0], ("q", "attention".to_string()));
    }

    fn searxng_body() -> Value {
        json!({
            "query": "x",
            "number_of_results": 0,
            "results": [
                { "url": "https://arxiv.org/abs/1", "title": "Paper", "content": "Abstract", "publishedDate": "2026-01-02T00:00:00", "engine": "google" },
                { "url": "https://example.com/a", "title": "Example", "content": "Snippet", "pubdate": "2026-02-03", "engine": "bing" },
                { "url": "https://example.com/b", "title": "No date" },
                { "title": "No url, so not a result" },
            ],
            "suggestions": [],
        })
    }

    #[test]
    fn results_take_the_perplexity_shape() {
        let r = parse(json!({ "query": "x" }));
        let results = r.shape_results(&searxng_body());
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0],
            json!({
                "title": "Paper",
                "url": "https://arxiv.org/abs/1",
                "snippet": "Abstract",
                "date": "2026-01-02T00:00:00",
                "last_updated": null,
            })
        );
        assert_eq!(results[1]["date"], "2026-02-03");
        assert_eq!(results[2]["snippet"], "");
        assert_eq!(results[2]["date"], Value::Null);
    }

    #[test]
    fn results_are_filtered_then_cut_to_max_results() {
        let r = parse(
            json!({ "query": "x", "max_results": 1, "search_domain_filter": ["example.com"] }),
        );
        let results = r.shape_results(&searxng_body());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://example.com/a");
        // A body with no results array is an empty answer, not an error.
        assert!(r.shape_results(&json!({ "unexpected": true })).is_empty());
    }

    fn search_candidate(name: &str, model_type: &str, enabled: bool) -> Candidate {
        let mut model = crate::boons::test_support::test_route();
        model.model_name = name.to_string();
        model.model_type = model_type.to_string();
        model.enabled = enabled;
        Candidate {
            model,
            healthy: true,
            levels: Vec::new(),
        }
    }

    #[test]
    fn tools_are_the_enabled_search_routes_a_caller_may_use() {
        let candidates = vec![
            search_candidate("web", "search", true),
            search_candidate("news", "search", true),
            search_candidate("retired", "search", false),
            search_candidate("glm-5-3", "chat", true),
        ];
        assert_eq!(tool_names(&candidates, None), vec!["news", "web"]);
        let allowed = vec!["web".to_string(), "glm-5-3".to_string()];
        assert_eq!(tool_names(&candidates, Some(&allowed)), vec!["web"]);
    }

    #[test]
    fn upstream_rejections_name_the_likely_cause() {
        assert_eq!(
            rejection(reqwest::StatusCode::TOO_MANY_REQUESTS).status,
            StatusCode::TOO_MANY_REQUESTS
        );
        let forbidden = rejection(reqwest::StatusCode::FORBIDDEN);
        assert_eq!(forbidden.status, StatusCode::BAD_GATEWAY);
        assert!(forbidden.message.contains("JSON output"));
        assert_eq!(
            rejection(reqwest::StatusCode::NOT_FOUND).status,
            StatusCode::BAD_GATEWAY
        );
    }
}

/// The endpoint end to end against a fake SearXNG: auth, tool resolution,
/// admission, the upstream call, the answer and the ledger row. Needs the
/// test datastores (`OBLETH_TEST_DATABASE_URL`, `OBLETH_TEST_REDIS_URL`) for
/// the `AppState`; skipped without them.
#[cfg(test)]
mod pipeline_tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU16, Ordering};
    use std::sync::Mutex;

    use axum::extract::Query;
    use axum::http::{HeaderMap, Method};
    use axum::routing::get;
    use axum::Router;
    use moka::future::Cache;

    use super::*;

    /// One call the fake saw: its query string and `Authorization` header.
    type Seen = (HashMap<String, String>, Option<String>);

    #[derive(Default)]
    struct FakeSearxng {
        seen: Mutex<Vec<Seen>>,
        /// Answer with this status instead of results when set.
        refuse_with: AtomicU16,
    }

    async fn fake_search(
        State(fake): State<Arc<FakeSearxng>>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Response<Body> {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        fake.seen.lock().unwrap().push((query, auth));
        match fake.refuse_with.load(Ordering::SeqCst) {
            0 => axum::Json(json!({
                "query": "attention",
                "results": [
                    { "url": "https://arxiv.org/abs/1706.03762", "title": "Attention Is All You Need", "content": "The dominant sequence transduction models…", "publishedDate": "2017-06-12T00:00:00" },
                    { "url": "https://example.com/a", "title": "A", "content": "a" },
                    { "url": "https://example.com/b", "title": "B", "content": "b" },
                ],
            }))
            .into_response(),
            code => (StatusCode::from_u16(code).unwrap(), "refused").into_response(),
        }
    }

    /// A fake SearXNG instance; returns it and its root URL.
    async fn spawn_searxng() -> (Arc<FakeSearxng>, String) {
        let fake = Arc::new(FakeSearxng::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/search", get(fake_search))
            .with_state(fake.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (fake, format!("http://{addr}"))
    }

    struct Gateway {
        state: AppState,
        wal_dir: std::path::PathBuf,
    }

    impl Drop for Gateway {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.wal_dir);
        }
    }

    impl Gateway {
        async fn register(&self, route: ResolvedModel) {
            self.state
                .model_cache
                .insert(route.model_name.clone(), Arc::new(route))
                .await;
        }

        async fn key(&self, allowed_models: Option<Vec<String>>) -> String {
            let mut key = crate::boons::test_support::test_key();
            key.key_id = Uuid::new_v4();
            key.tenant_id = Uuid::new_v4();
            key.allowed_models = allowed_models;
            let secret = format!("sk-search-{}", Uuid::new_v4());
            self.state
                .key_cache
                .insert(obleth_config::hash_api_key(&secret), Arc::new(key))
                .await;
            secret
        }

        fn ledger_rows(&self) -> u64 {
            self.state
                .telemetry
                .stats()
                .recorded
                .load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    async fn gateway() -> Option<Gateway> {
        let (Ok(db), Ok(redis_url)) = (
            std::env::var("OBLETH_TEST_DATABASE_URL"),
            std::env::var("OBLETH_TEST_REDIS_URL"),
        ) else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return None;
        };
        let store = obleth_store::Store::connect(&db).await.expect("postgres");
        let redis = obleth_redis::RedisStore::connect(&redis_url)
            .await
            .expect("redis");
        let wal_dir = std::env::temp_dir().join(format!("obleth-search-wal-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&wal_dir).unwrap();
        let telemetry = obleth_telemetry::TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth_test",
            "default",
            "",
            wal_dir.join("usage.wal").to_str().unwrap(),
            Duration::from_millis(100),
            true,
        )
        .await
        .expect("telemetry sink");
        let http = reqwest::Client::new();
        let state = AppState {
            redis,
            fairshare: obleth_fairshare::FairShare::start(
                Arc::new(obleth_fairshare::StaticCapacity::new(64)),
                obleth_config::FairshareAlgorithm::default(),
                64,
            ),
            tokenizer: Arc::new(obleth_tokenizer::HeuristicTokenizer::new()),
            telemetry,
            http: http.clone(),
            upstream_base: "http://127.0.0.1:1/v1".into(),
            upstream_timeout: Duration::from_secs(30),
            key_cache: Cache::builder().build(),
            model_cache: Cache::builder().build(),
            mcp_cache: Cache::builder().build(),
            model_registry: crate::router::ModelRegistry::new(),
            classifier: crate::classifier::Classifier::new(Default::default()),
            output_stats: Default::default(),
            boons: crate::boons::BoonEngine::new(Default::default()),
            tool_cache: Cache::builder().build(),
            metrics: Arc::new(crate::metrics::Metrics::new()),
            fail_open: false,
            alerts: obleth_admin::AlertDispatcher::new(http, Default::default()),
            session_id_derivation: false,
            compressor: None,
            energy: crate::energy::EnergyEngine::new(Default::default()),
            jwt: None,
            knowledge: Arc::new(crate::knowledge::KnowledgeIndex::new()),
            video_jobs: crate::videos::VideoJobStore::new(store, Default::default()),
        };
        Some(Gateway { state, wal_dir })
    }

    fn search_route(name: &str, base: &str) -> ResolvedModel {
        let mut route = crate::boons::test_support::test_route();
        route.model_name = name.into();
        route.upstream_model = "searxng".into();
        route.api_base = base.into();
        route.api_key = Some("up-key".into());
        route.model_type = SEARCH_MODEL_TYPE.into();
        route
    }

    fn post(uri: &str, secret: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {secret}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn json_of(resp: Response<Body>) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    #[tokio::test]
    async fn a_search_reaches_searxng_and_answers_in_the_perplexity_shape() {
        let Some(gw) = gateway().await else { return };
        let (fake, base) = spawn_searxng().await;
        gw.register(search_route("web-search", &base)).await;
        let secret = gw.key(None).await;
        let rows = gw.ledger_rows();

        let resp = search_named(
            State(gw.state.clone()),
            Path("web-search".into()),
            post(
                "/v1/search/web-search",
                &secret,
                json!({ "query": "attention", "max_results": 2, "country": "US" }),
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().contains_key("x-obleth-request-id"));
        let body = json_of(resp).await;
        assert_eq!(body["object"], "search");
        let results = body["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"], "Attention Is All You Need");
        assert_eq!(
            results[0]["snippet"],
            "The dominant sequence transduction models…"
        );
        assert_eq!(results[0]["date"], "2017-06-12T00:00:00");

        let seen = fake.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        let (query, auth) = &seen[0];
        assert_eq!(query["q"], "attention");
        assert_eq!(query["format"], "json");
        assert_eq!(query["language"], "en-US");
        assert_eq!(auth.as_deref(), Some("Bearer up-key"));
        assert_eq!(gw.ledger_rows(), rows + 1);
    }

    #[tokio::test]
    async fn an_unnamed_search_uses_the_callers_only_tool() {
        let Some(gw) = gateway().await else { return };
        let (fake, base) = spawn_searxng().await;
        let route = search_route("web-search", &base);
        gw.state.model_registry.store(vec![Candidate {
            model: route.clone(),
            healthy: true,
            levels: Vec::new(),
        }]);
        gw.register(route).await;
        let secret = gw.key(None).await;

        let resp = search(
            State(gw.state.clone()),
            post("/v1/search", &secret, json!({ "query": "attention" })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_of(resp).await["results"].as_array().unwrap().len(), 3);
        assert_eq!(fake.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn only_a_permitted_search_tool_is_searched() {
        let Some(gw) = gateway().await else { return };
        let (fake, base) = spawn_searxng().await;
        gw.register(search_route("web-search", &base)).await;
        let mut chat = crate::boons::test_support::test_route();
        chat.model_name = "glm-5-3".into();
        gw.register(chat).await;

        // A chat model is not a search tool.
        let secret = gw.key(None).await;
        let resp = search(
            State(gw.state.clone()),
            post(
                "/v1/search",
                &secret,
                json!({ "query": "x", "search_tool_name": "glm-5-3" }),
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // A tenant allowlist bounds search tools as it bounds models.
        let limited = gw.key(Some(vec!["glm-5-3".into()])).await;
        let resp = search_named(
            State(gw.state.clone()),
            Path("web-search".into()),
            post("/v1/search/web-search", &limited, json!({ "query": "x" })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // And a chat call aimed at the search tool is refused, not forwarded.
        let resp = crate::proxy::proxy_handler(
            State(gw.state.clone()),
            post(
                "/v1/chat/completions",
                &secret,
                json!({ "model": "web-search", "messages": [{ "role": "user", "content": "hi" }] }),
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(json_of(resp).await["error"]["message"]
            .as_str()
            .unwrap()
            .contains("search tool"));
        assert!(fake.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refused_json_format_is_reported_as_the_gateways_fault() {
        let Some(gw) = gateway().await else { return };
        let (fake, base) = spawn_searxng().await;
        fake.refuse_with.store(403, Ordering::SeqCst);
        gw.register(search_route("web-search", &base)).await;
        let secret = gw.key(None).await;
        let rows = gw.ledger_rows();

        let resp = search_named(
            State(gw.state.clone()),
            Path("web-search".into()),
            post("/v1/search/web-search", &secret, json!({ "query": "x" })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        assert!(json_of(resp).await["error"]["message"]
            .as_str()
            .unwrap()
            .contains("JSON output"));
        // The failure is on the ledger like any other.
        assert_eq!(gw.ledger_rows(), rows + 1);
    }
}
