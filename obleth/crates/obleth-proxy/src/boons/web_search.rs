//! The **web-search** boon: lets a chat model look things up on the web.
//!
//! A model granted this boon has a `web_search` function tool merged into its
//! chat request. When the model calls it, the gateway runs the search itself
//! against the configured `search`-type route — the same upstream call,
//! failover and result shaping as `POST /v1/search` ([`crate::search`]), with
//! no MCP server involved, exactly like the image-generation boon's
//! `generate_image`.
//!
//! The results come back to the model as a compact numbered list (title, URL,
//! date, snippet) in a `role: "tool"` message, and the model writes the answer
//! citing the URLs it used. Every search is recorded as its own usage row
//! linked to the chat request (`parent_request_id`), so a request's searches
//! show up under it.
//!
//! Fail-open throughout: no search tool, a disabled route, an upstream error,
//! a timeout, a guardrails block, or the per-request limit all become a short
//! tool result the model can answer around. A search never fails the request.

use std::collections::HashMap;
use std::time::Duration;

use obleth_config::{
    ResolvedKey, WebSearchBoonSettings, SEARCH_MODEL_TYPE, WEB_SEARCH_MAX_PER_REQUEST,
    WEB_SEARCH_MAX_RESULTS,
};
use serde_json::{json, Value};

use crate::state::AppState;

/// Name of the gateway-executed tool this boon injects.
pub(super) const WEB_SEARCH_TOOL: &str = "web_search";

/// Synthetic "server" name registered in the tool map for `web_search`, so the
/// loop recognizes the tool as gateway-owned (not a client tool to pass
/// through). Never used as a real MCP server; `execute_call` short-circuits the
/// call before any server lookup.
pub(super) const SEARCH_SYNTHETIC_SERVER: &str = "__web_search__";

/// Longest query sent upstream. Models occasionally paste a whole question or
/// paragraph into `query`; a search engine wants keywords.
const QUERY_MAX_CHARS: usize = 400;

/// Domains the model may restrict one search to.
const MAX_TOOL_DOMAINS: usize = 10;

/// Longest snippet handed to the model per result.
const SNIPPET_MAX_CHARS: usize = 400;

/// Results handed to the model per search: at least 1, never above the hard
/// ceiling.
pub(super) fn max_results(cfg: &WebSearchBoonSettings) -> u32 {
    cfg.max_results.clamp(1, WEB_SEARCH_MAX_RESULTS)
}

/// Searches one request may run: at least 1, never above the hard ceiling.
pub(super) fn max_searches(cfg: &WebSearchBoonSettings) -> u32 {
    cfg.max_searches_per_request
        .clamp(1, WEB_SEARCH_MAX_PER_REQUEST)
}

/// The OpenAI function-tool definition for `web_search`.
pub(super) fn tool_def(cfg: &WebSearchBoonSettings) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": WEB_SEARCH_TOOL,
            "description": cfg.tool_description,
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "A short search-engine query: the specific \
                            keywords to look up, not a whole question.",
                    },
                    "domains": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional. Only return results from these \
                            sites, for example [\"arxiv.org\"].",
                    },
                },
                "required": ["query"]
            }
        }
    })
}

/// The short system nudge injected for plain chat clients that brought no tools
/// of their own.
fn nudge_text() -> &'static str {
    "A `web_search` tool is available. Call it when the answer depends on current or \
     external information you are not sure of, and cite the URLs of the results you use. \
     Answer directly, without searching, when you already know the answer. This tool is \
     background capability, not your purpose: do not mention or offer web searching \
     unless it helps answer the user."
}

/// Whether `web_search` is already owned by something else: a tool the client
/// itself defined (agentic clients often bring their own `web_search`), or a
/// tool the MCP tool loop already mapped from a granted server. The existing
/// owner always wins.
///
/// As with `image_gen::collides`, the caller must decide the injection and the
/// `tool_loop_servers` map insert together from this single answer, so the
/// request body and the loop's dispatch map never disagree about who owns the
/// name.
pub(super) fn collides(
    json_body: &Value,
    tool_loop_servers: Option<&HashMap<String, String>>,
) -> bool {
    let client_has_it = json_body
        .get("tools")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| {
            arr.iter().any(|t| {
                t.pointer("/function/name").and_then(|n| n.as_str()) == Some(WEB_SEARCH_TOOL)
            })
        });
    client_has_it || tool_loop_servers.is_some_and(|m| m.contains_key(WEB_SEARCH_TOOL))
}

/// Merge the tool definition into the request, and optionally add the system
/// nudge. Client-supplied tools are preserved.
///
/// Callers must check [`collides`] first: this function does not itself guard
/// against a name collision.
pub(super) fn inject(
    cfg: &WebSearchBoonSettings,
    nudge: bool,
    supports_system_messages: bool,
    json_body: &mut Value,
) {
    let def = tool_def(cfg);
    if let Some(obj) = json_body.as_object_mut() {
        match obj.get_mut("tools").and_then(|v| v.as_array_mut()) {
            Some(existing) => existing.push(def),
            None => {
                obj.insert("tools".into(), Value::Array(vec![def]));
            }
        }
    }
    if nudge {
        super::structured::inject_prompt_section(json_body, nudge_text(), supports_system_messages);
    }
}

/// `text` cut to `max` characters on a char boundary, with an ellipsis when
/// anything was dropped.
fn shorten(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut s: String = trimmed.chars().take(max).collect();
    s.push('…');
    s
}

/// The trimmed `query` argument, shortened to [`QUERY_MAX_CHARS`], or `None`
/// when the model omitted it or sent only whitespace. `q` is accepted too:
/// models trained on other search tools reach for it.
pub(super) fn query_arg(args: &Value) -> Option<String> {
    args.get("query")
        .or_else(|| args.get("q"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| {
            let flat = q.split_whitespace().collect::<Vec<_>>().join(" ");
            shorten(&flat, QUERY_MAX_CHARS)
                .trim_end_matches('…')
                .to_string()
        })
}

/// The `domains` argument as a list of non-empty strings, at most
/// [`MAX_TOOL_DOMAINS`]. A single string is accepted as a one-item list.
pub(super) fn domains_arg(args: &Value) -> Vec<String> {
    let raw: Vec<&str> = match args.get("domains") {
        Some(Value::Array(items)) => items.iter().filter_map(|v| v.as_str()).collect(),
        Some(Value::String(s)) => vec![s.as_str()],
        _ => Vec::new(),
    };
    raw.into_iter()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .take(MAX_TOOL_DOMAINS)
        .map(str::to_string)
        .collect()
}

/// The tool result for a search that came back: a numbered list the model can
/// cite from, or a plain statement that nothing was found.
pub(super) fn format_results(query: &str, results: &[Value]) -> String {
    if results.is_empty() {
        return format!(
            "No web results were found for \"{query}\". Answer from what you know, and say \
             that the search found nothing."
        );
    }
    let text = |r: &Value, key: &str| {
        r.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let mut out = format!("Web results for \"{query}\":\n");
    for (i, r) in results.iter().enumerate() {
        let title = text(r, "title");
        let url = text(r, "url");
        let snippet = shorten(&text(r, "snippet"), SNIPPET_MAX_CHARS);
        out.push_str(&format!(
            "\n[{}] {}\n{}\n",
            i + 1,
            if title.is_empty() {
                "(untitled)"
            } else {
                &title
            },
            url
        ));
        if let Some(date) = r.get("date").and_then(Value::as_str).map(str::trim) {
            if !date.is_empty() {
                out.push_str(&format!("Published: {date}\n"));
            }
        }
        if !snippet.is_empty() {
            out.push_str(&snippet);
            out.push('\n');
        }
    }
    out.push_str("\nCite the URLs of the results you rely on.");
    out
}

/// The tool result when a search did not run or did not answer. Reads as a
/// fact the model can answer around, not as an error to retry.
pub(super) fn failure_result(reason: &str) -> String {
    format!(
        "The web search could not be completed ({reason}). Answer from what you know, and \
         tell the user the search did not work; do not retry the tool."
    )
}

/// The tool result once the request has used its searches.
pub(super) fn limit_result(limit: u32) -> String {
    let plural = if limit == 1 { "search" } else { "searches" };
    format!(
        "Not searched: this request has already used its {limit} {plural}. Answer with the \
         results you already have."
    )
}

/// One search attempt, for the trace span. Recorded by the caller, which owns
/// the tracer (`execute` does not).
pub(super) struct SearchEvent {
    pub results: u32,
    pub tool: String,
    pub upstream_ms: u32,
    pub ok: bool,
}

/// Everything the gateway-executed `web_search` tool needs, threaded through
/// the tool loop for the life of one request.
pub(super) struct SearchCtx<'a> {
    /// Settings snapshot taken at request time, so a hot-reload mid-request
    /// cannot change behaviour.
    pub cfg: &'a WebSearchBoonSettings,
    pub key: &'a ResolvedKey,
    pub session_id: &'a str,
    /// The client request the searches run for; each search's usage row
    /// records it.
    pub request_id: uuid::Uuid,
    /// Searches attempted so far. Counts attempts, not successes, so a
    /// failing upstream cannot be retried past the limit.
    pub searches: u32,
    pub events: Vec<SearchEvent>,
}

/// Timeout for one search: the boon's own timeout, capped by the loop's
/// remaining time budget, and `None` once that is spent.
pub(super) fn call_timeout(
    cfg: &WebSearchBoonSettings,
    deadline: &super::tool_loop::LoopDeadline,
) -> Option<Duration> {
    deadline.bound(Duration::from_millis(cfg.timeout_ms.max(1)))
}

/// Execute one `web_search` call: check the limit, resolve the search route,
/// apply the tenant's input guardrails to the query (it leaves for the open
/// web), run the search, record it, and return the text the model will read.
pub(super) async fn execute(
    state: &AppState,
    ctx: &mut SearchCtx<'_>,
    args: &Value,
    timeout: Duration,
) -> String {
    let Some(query) = query_arg(args) else {
        return failure_result("no query was supplied");
    };
    let limit = max_searches(ctx.cfg);
    if ctx.searches >= limit {
        return limit_result(limit);
    }
    let Some(tool_name) = ctx
        .cfg
        .search_tool
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        return failure_result("no search tool is configured");
    };
    let Some(route) = crate::proxy::resolve_model(state, tool_name).await else {
        tracing::warn!(
            tool = %tool_name,
            "web-search boon target is not registered; no search run"
        );
        return failure_result("the search tool is not available");
    };
    if !route.enabled || route.model_type != SEARCH_MODEL_TYPE {
        tracing::warn!(
            tool = %tool_name,
            model_type = %route.model_type,
            enabled = route.enabled,
            "web-search boon target is disabled or not a search route; no search run"
        );
        return failure_result("the search tool is not available");
    }

    // The query leaves for the open web, so the tenant's input policy applies
    // to it exactly as `POST /v1/search` applies it. A redaction is searched
    // as redacted; a block means no search.
    let mut scan = json!({ "messages": [{ "role": "user", "content": query }] });
    let query = match state
        .boons
        .scan_input(
            state,
            ctx.key,
            ctx.session_id,
            ctx.request_id,
            &mut scan,
            None,
        )
        .await
    {
        Err(_) => return failure_result("the query was blocked by policy"),
        Ok(true) => match scan["messages"][0]["content"].as_str() {
            Some(redacted) => redacted.to_string(),
            None => return failure_result("the query was blocked by policy"),
        },
        Ok(false) => query,
    };

    ctx.searches += 1;
    let domains = domains_arg(args);
    let domain_refs: Vec<&str> = domains.iter().map(String::as_str).collect();
    let started = crate::tracer::now_ms();
    let outcome = tokio::time::timeout(
        timeout,
        crate::search::search_for_boon(
            state,
            &route,
            &query,
            max_results(ctx.cfg) as usize,
            &domain_refs,
            ctx.session_id,
        ),
    )
    .await;
    let upstream_ms = (crate::tracer::now_ms() - started) as u32;

    let (status, result, results) = match outcome {
        Ok(Ok(results)) => {
            let count = results.len() as u32;
            (200, format_results(&query, &results), count)
        }
        Ok(Err(reason)) => {
            tracing::warn!(
                tool = %route.model_name,
                %reason,
                "web-search boon search failed; answering without results"
            );
            (502, failure_result(&reason), 0)
        }
        Err(_) => {
            tracing::warn!(
                tool = %route.model_name,
                timeout_ms = timeout.as_millis() as u64,
                "web-search boon search timed out; answering without results"
            );
            (504, failure_result("timed out"), 0)
        }
    };
    super::bill_web_search(
        state,
        &route,
        ctx.key,
        ctx.session_id,
        ctx.request_id,
        status,
        upstream_ms,
    );
    ctx.events.push(SearchEvent {
        results,
        tool: route.model_name.clone(),
        upstream_ms,
        ok: status == 200,
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> WebSearchBoonSettings {
        WebSearchBoonSettings {
            enabled: true,
            search_tool: Some("web".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn the_tool_definition_asks_for_a_query_and_optional_domains() {
        let def = tool_def(&cfg());
        assert_eq!(def["function"]["name"], WEB_SEARCH_TOOL);
        assert_eq!(def["function"]["parameters"]["required"], json!(["query"]));
        assert!(def["function"]["parameters"]["properties"]["domains"].is_object());
        assert_eq!(
            def["function"]["description"],
            obleth_config::DEFAULT_WEB_SEARCH_TOOL_DESCRIPTION
        );
    }

    #[test]
    fn limits_are_clamped_to_the_hard_ceilings() {
        let mut c = cfg();
        c.max_results = 0;
        c.max_searches_per_request = 0;
        assert_eq!(max_results(&c), 1);
        assert_eq!(max_searches(&c), 1);
        c.max_results = 500;
        c.max_searches_per_request = 500;
        assert_eq!(max_results(&c), WEB_SEARCH_MAX_RESULTS);
        assert_eq!(max_searches(&c), WEB_SEARCH_MAX_PER_REQUEST);
    }

    #[test]
    fn a_client_or_mcp_web_search_wins_over_the_boon() {
        let plain = json!({ "messages": [] });
        assert!(!collides(&plain, None));

        let client = json!({
            "tools": [{ "type": "function", "function": { "name": "web_search" } }]
        });
        assert!(collides(&client, None));

        let mut mcp = HashMap::new();
        mcp.insert("web_search".to_string(), "searxng-mcp".to_string());
        assert!(collides(&plain, Some(&mcp)));

        let mut other = HashMap::new();
        other.insert("generate_image".to_string(), "__image__".to_string());
        assert!(!collides(&plain, Some(&other)));
    }

    #[test]
    fn inject_keeps_client_tools_and_nudges_only_when_asked() {
        let mut body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "function", "function": { "name": "client_tool" } }]
        });
        inject(&cfg(), false, true, &mut body);
        let names: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert_eq!(names, vec!["client_tool", WEB_SEARCH_TOOL]);
        assert_eq!(body["messages"].as_array().unwrap().len(), 1, "no nudge");

        let mut plain = json!({ "messages": [{ "role": "user", "content": "hi" }] });
        inject(&cfg(), true, true, &mut plain);
        assert_eq!(plain["tools"].as_array().unwrap().len(), 1);
        assert!(plain
            .to_string()
            .contains("A `web_search` tool is available"));
    }

    #[test]
    fn query_arg_trims_flattens_and_caps() {
        assert_eq!(query_arg(&json!({})), None);
        assert_eq!(query_arg(&json!({ "query": "   " })), None);
        assert_eq!(
            query_arg(&json!({ "query": "  gaudi 3\n memory  " })).as_deref(),
            Some("gaudi 3 memory")
        );
        assert_eq!(query_arg(&json!({ "q": "vllm" })).as_deref(), Some("vllm"));
        let long = "word ".repeat(200);
        let q = query_arg(&json!({ "query": long })).unwrap();
        assert_eq!(q.chars().count(), QUERY_MAX_CHARS);
        assert!(!q.ends_with('…'));
    }

    #[test]
    fn domains_arg_accepts_a_list_or_one_string() {
        assert!(domains_arg(&json!({})).is_empty());
        assert_eq!(
            domains_arg(&json!({ "domains": ["arxiv.org", " ", "asu.edu"] })),
            vec!["arxiv.org".to_string(), "asu.edu".to_string()]
        );
        assert_eq!(
            domains_arg(&json!({ "domains": "asu.edu" })),
            vec!["asu.edu".to_string()]
        );
        let many: Vec<String> = (0..30).map(|i| format!("d{i}.org")).collect();
        assert_eq!(
            domains_arg(&json!({ "domains": many })).len(),
            MAX_TOOL_DOMAINS
        );
    }

    #[test]
    fn results_read_as_a_numbered_citable_list() {
        let results = vec![
            json!({
                "title": "Gaudi 3",
                "url": "https://example.org/gaudi3",
                "snippet": "128 GB of HBM",
                "date": "2024-04-09",
            }),
            json!({
                "title": "",
                "url": "https://example.org/x",
                "snippet": "s".repeat(1000),
                "date": null,
            }),
        ];
        let text = format_results("gaudi 3 memory", &results);
        assert!(text.starts_with("Web results for \"gaudi 3 memory\":"));
        assert!(text.contains(
            "[1] Gaudi 3\nhttps://example.org/gaudi3\nPublished: 2024-04-09\n128 GB of HBM"
        ));
        assert!(text.contains("[2] (untitled)\nhttps://example.org/x\n"));
        assert!(!text.contains(&"s".repeat(SNIPPET_MAX_CHARS + 1)));
        assert!(text.ends_with("Cite the URLs of the results you rely on."));
    }

    #[test]
    fn no_results_and_failures_tell_the_model_to_answer_anyway() {
        assert!(format_results("q", &[]).contains("No web results were found"));
        let failed = failure_result("timed out");
        assert!(failed.contains("(timed out)"));
        assert!(failed.contains("do not retry"));
        assert!(limit_result(1).contains("its 1 search."));
        assert!(limit_result(3).contains("its 3 searches."));
    }
}
