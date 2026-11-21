//! Which gateway paths exist and what each one is: the canonical form of a
//! request path, the endpoints forwarded upstream, the ones that need a
//! registered model, and the request type a path records as.

use super::*;

/// The canonical spelling of a request path, when it differs from the request's.
///
/// Repeated and trailing slashes are removed and a repeated version prefix
/// (`/v1/v1/…`, what a client sends when its base URL already ends in `/v1`) is
/// read as one. For POST, the bare `/responses`, `/messages`,
/// `/messages/count_tokens`, and `/verdicts` spellings map to their `/v1` forms.
///
/// Every method is canonicalized, once, before anything looks at the path: the
/// shims, the job, file and batch routes, the request type, and the upstream
/// URL all read the same spelling. When only POST was, `GET //v1/videos` or
/// `/v1/v1/videos/{id}` matched no job route, skipped its ownership check, and
/// was forwarded to the backend, whose URL builder dropped the extra slash or
/// prefix; `/v1/responses/` or `/responses` skipped the translation shims, so
/// a tenant's guardrails never saw it, and `/v1/chat/completions/` lost its
/// chat classification (and with it output guardrails and every chat-only boon).
pub(super) fn canonical_path(method: &Method, path: &str) -> Option<String> {
    // Runs of `/` collapse to one, so `//v1/responses` cannot skip the shim.
    let mut collapsed = String::with_capacity(path.len());
    for c in path.chars() {
        if !(c == '/' && collapsed.ends_with('/')) {
            collapsed.push(c);
        }
    }
    let mut trimmed = collapsed.trim_end_matches('/');
    while let Some(rest) = trimmed.strip_prefix("/v1/v1") {
        if !(rest.is_empty() || rest.starts_with('/')) {
            break;
        }
        // Drops the first `/v1`; what is left still starts with one.
        trimmed = &trimmed["/v1".len()..];
    }
    let post = method == Method::POST;
    let mapped = match trimmed {
        "/responses" if post => crate::responses::RESPONSES_PATH,
        "/messages" if post => crate::messages::MESSAGES_PATH,
        "/messages/count_tokens" if post => crate::messages::COUNT_TOKENS_PATH,
        "/verdicts" if post => crate::verdicts::VERDICTS_PATH,
        other => other,
    };
    (!mapped.is_empty() && mapped != path).then(|| mapped.to_string())
}

/// True for the model-listing collection itself, in either spelling. The
/// trailing slash carries no id, so `/v1/models/` lists rather than being read
/// as a detail lookup for the empty string.
pub(super) fn is_models_collection(path: &str) -> bool {
    path == "/v1/models" || path == "/v1/models/"
}

/// The OpenAI model-discovery endpoints: `GET /v1/models` (list), the
/// `GET /v1/models/{id}` / `{"model": …}` detail probe, and obleth's
/// registry-backed `/model/info`. These are the only paths the gateway serves
/// without resolving to a registered model, so they are exempt from the
/// unmapped-path rejection.
pub(super) fn is_models_endpoint(path: &str) -> bool {
    path == "/v1/models" || path.starts_with("/v1/models/") || is_model_info_endpoint(path)
}

/// The `/model/info` detail listing, accepted both bare and under `/v1` —
/// LiteLLM serves it at the bare path, and a client that appends the OpenAI
/// `/v1` prefix to every request should not be the one to find that out.
pub(super) fn is_model_info_endpoint(path: &str) -> bool {
    path == "/model/info" || path == "/v1/model/info"
}

/// The inference endpoints the gateway forwards to a model's upstream, in their
/// canonical spelling. Each is a POST that must name a registered model. The
/// request type, and so the guardrails and metering a call gets, is read from
/// the path's ending (`request_type_for_path`), which is safe only because no
/// other path with that ending is forwarded.
pub(super) const REGISTERED_MODEL_PATHS: &[&str] = &[
    "/v1/chat/completions",
    "/v1/completions",
    "/v1/embeddings",
    "/v1/responses",
    "/v1/audio/transcriptions",
    "/v1/audio/translations",
    "/v1/audio/speech",
    "/v1/images/generations",
    "/v1/images/edits",
    "/v1/images/variations",
    crate::jobs::VIDEOS_PATH,
    crate::jobs::MESHES_PATH,
    "/v1/rerank",
    "/v1/reranking",
    "/rerank",
    "/reranking",
    "/v1/moderations",
    "/v1/score",
    "/score",
    "/v1/tokenize",
    "/v1/detokenize",
    "/tokenize",
    "/detokenize",
    "/v1/pooling",
    "/pooling",
    "/v1/classify",
    "/classify",
];

pub(super) fn requires_registered_model(path: &str) -> bool {
    REGISTERED_MODEL_PATHS.contains(&path)
}

/// Whether the pipeline may send this request on to an upstream: an inference
/// endpoint or model discovery, and nothing else.
pub(super) fn is_forwarded(method: &Method, path: &str) -> bool {
    (method == Method::POST && requires_registered_model(path))
        || (method == Method::GET && is_models_endpoint(path))
}

/// Classify a request by its OpenAI-style path suffix. Matching the suffix (not
/// the full path) keeps this robust to version prefixes (`/v1/...`) or any
/// future routing prefix.
pub(super) fn request_type_for_path(path: &str) -> &'static str {
    if path.ends_with("/chat/completions") {
        "chat"
    } else if path.ends_with("/responses") {
        "responses"
    } else if path.ends_with("/verdicts") {
        "verdict"
    } else if path.ends_with("/completions") {
        "completion"
    } else if path.ends_with("/embeddings") {
        "embedding"
    } else if path.contains("/audio/") {
        "audio"
    } else if path.contains("/images/") {
        "image"
    } else if path.ends_with("/videos") {
        "video"
    } else if path.ends_with("/meshes") {
        "mesh"
    } else if path.ends_with("/rerank") || path.ends_with("/reranking") {
        "rerank"
    } else if path.ends_with("/moderations") {
        "moderation"
    } else if crate::search::is_search_path(path) {
        "search"
    } else {
        "other"
    }
}

/// The request class recorded in the ledger: synthetic tenants' traffic is
/// tagged `benchmark` (replacing the path-derived class, mirroring how health
/// probes record `health_probe`), everything else classifies by path.
///
/// This is an ACCOUNTING label. It must never gate behavior — see
/// [`is_chat_path`].
fn effective_request_type(resolved: &ResolvedKey, path: &str) -> &'static str {
    if resolved.synthetic {
        obleth_config::BENCHMARK_REQUEST_TYPE
    } else {
        request_type_for_path(path)
    }
}

/// [`effective_request_type`], except a request either shim translated is
/// recorded by its own surface (`responses`, `messages`). Both run down the
/// chat path by design, so the path alone would report the caller's surface
/// as chat and make adoption of the new APIs invisible in the ledger.
pub(crate) fn surfaced_request_type(
    resolved: &ResolvedKey,
    path: &str,
    surface: Option<&str>,
) -> &'static str {
    if !resolved.synthetic {
        match surface {
            Some(crate::responses::SURFACE) => return "responses",
            Some(crate::messages::SURFACE) => return "messages",
            _ => {}
        }
    }
    effective_request_type(resolved, path)
}

/// Whether chat-only boons (knowledge, compression, tools, structured output,
/// the MCP tool loop) apply to this request.
///
/// Derived from the path, never from the ledger's `request_type`: that label is
/// overwritten with `benchmark` for every synthetic tenant, so reading it here
/// disabled every chat-only boon for precisely the two callers that exist to
/// exercise them — the Charo console/playground and the benchmark suite, both of
/// which call the data plane as the reserved (synthetic) control-plane tenant.
pub(super) fn is_chat_path(path: &str) -> bool {
    request_type_for_path(path) == "chat"
}

/// Endpoints whose streams honour `stream_options.include_usage`.
pub(super) fn is_stream_usage_path(path: &str) -> bool {
    matches!(request_type_for_path(path), "chat" | "completion")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::tests::key_with_schedule;

    #[test]
    fn every_method_reads_one_spelling_of_a_path() {
        for method in [Method::GET, Method::DELETE, Method::POST] {
            for (sent, read) in [
                ("//v1/videos", "/v1/videos"),
                ("/v1//videos/vid_1/content", "/v1/videos/vid_1/content"),
                ("/v1/videos/", "/v1/videos"),
                ("/v1/v1/videos/vid_1", "/v1/videos/vid_1"),
                ("/v1/v1/v1/responses", "/v1/responses"),
                ("//v1//v1/batches/b_1", "/v1/batches/b_1"),
                ("/v1/v1", "/v1"),
            ] {
                assert_eq!(
                    canonical_path(&method, sent).as_deref(),
                    Some(read),
                    "{method} {sent}"
                );
            }
            assert_eq!(canonical_path(&method, "/v1/videos"), None);
            assert_eq!(canonical_path(&method, "/v1/v1beta/x"), None);
        }
        // The bare spellings are POST endpoints.
        assert_eq!(canonical_path(&Method::GET, "/responses"), None);
        // What skipped the job routes now reaches them.
        for sent in ["//v1/videos", "/v1/v1/videos"] {
            let read = canonical_path(&Method::GET, sent).unwrap();
            assert!(
                crate::jobs::follow_up(&Method::GET, &read).is_some(),
                "{sent}"
            );
        }
        let read = canonical_path(&Method::DELETE, "/v1/v1/videos/vid_1").unwrap();
        assert!(crate::jobs::follow_up(&Method::DELETE, &read).is_some());
    }

    #[test]
    fn only_known_endpoints_are_forwarded() {
        for path in REGISTERED_MODEL_PATHS {
            assert!(is_forwarded(&Method::POST, path), "{path}");
            for method in [Method::GET, Method::DELETE, Method::PUT, Method::PATCH] {
                assert!(!is_forwarded(&method, path), "{method} {path}");
            }
        }
        for path in ["/v1/models", "/v1/models/gpt-4o", "/model/info"] {
            assert!(is_forwarded(&Method::GET, path), "{path}");
            assert!(!is_forwarded(&Method::POST, path), "{path}");
            assert!(!is_forwarded(&Method::DELETE, path), "{path}");
        }
        // Whatever else an upstream serves, and endpoints under another prefix.
        for (method, path) in [
            (Method::GET, "/stats"),
            (Method::GET, "/v1/files"),
            (Method::GET, "/v1/chat/completions"),
            (Method::POST, "/v1/fine_tuning/jobs"),
            (Method::POST, "/v1/files"),
            (Method::POST, "/internal/chat/completions"),
            (Method::POST, "/v1/audio/anything"),
            (Method::POST, "/v1/videos/vid_1/remix"),
            (Method::POST, "/"),
        ] {
            assert!(!is_forwarded(&method, path), "{method} {path}");
        }
    }

    #[test]
    fn synthetic_tenant_requests_are_tagged_benchmark() {
        let mut resolved = key_with_schedule("UTC", None, None, None);
        assert_eq!(
            effective_request_type(&resolved, "/v1/chat/completions"),
            "chat"
        );
        resolved.synthetic = true;
        assert_eq!(
            effective_request_type(&resolved, "/v1/chat/completions"),
            obleth_config::BENCHMARK_REQUEST_TYPE
        );
    }

    #[test]
    fn a_synthetic_tenants_chat_request_still_counts_as_chat_for_boons() {
        // The ledger label and the boon gate must not be the same value. A
        // synthetic tenant's chat completion is stamped `benchmark` for
        // accounting, but it is still a chat request: every chat-only boon
        // (knowledge, compression, tools, structured output, the MCP tool loop)
        // has to fire for it, or the console and the benchmark suite measure a
        // gateway with those boons silently switched off.
        let mut resolved = key_with_schedule("UTC", None, None, None);
        resolved.synthetic = true;
        assert_eq!(
            effective_request_type(&resolved, "/v1/chat/completions"),
            obleth_config::BENCHMARK_REQUEST_TYPE,
            "accounting still tags it benchmark"
        );
        assert!(
            is_chat_path("/v1/chat/completions"),
            "but the boon gate reads the path, not that label"
        );
        assert!(!is_chat_path("/v1/embeddings"));
        assert!(!is_chat_path("/v1/images/generations"));
    }

    #[test]
    fn verdict_requests_are_classified_by_path() {
        // `/v1/verdicts` is served by its own route (see main.rs), but the
        // ledger label still derives from the path like every other class.
        assert_eq!(request_type_for_path("/v1/verdicts"), "verdict");
        assert_eq!(request_type_for_path("/v1/search"), "search");
        assert_eq!(request_type_for_path("/v1/search/web"), "search");
        // Another API's search sub-resource is not ours.
        assert_eq!(
            request_type_for_path("/v1/vector_stores/vs_1/search"),
            "other"
        );
        // Not confused with the legacy completions suffix match.
        assert_eq!(request_type_for_path("/v1/completions"), "completion");
    }

    #[test]
    fn models_collection_covers_both_spellings_but_not_a_detail_path() {
        assert!(is_models_collection("/v1/models"));
        // The slash carries no id: this lists, it does not look up "".
        assert!(is_models_collection("/v1/models/"));

        // A real id is a detail lookup, not the collection.
        assert!(!is_models_collection("/v1/models/glm-5-3"));
        assert!(!is_models_collection("/v1/models/glm-5-3/"));
        assert!(!is_models_collection("/v1/model/info"));
        assert!(!is_models_collection("/v1/chat/completions"));
    }

    #[test]
    fn both_collection_spellings_are_model_discovery_endpoints() {
        // Both must stay reachable without resolving a model, or the listing
        // is rejected as an unmapped path before it can be served.
        assert!(is_models_endpoint("/v1/models"));
        assert!(is_models_endpoint("/v1/models/"));
    }

    #[test]
    fn unmapped_paths_are_rejected_not_forwarded() {
        // The handler rejects a request when it resolved to no registered model
        // (route None), the path is not a recognized OpenAI endpoint, and it is
        // not a model-discovery endpoint. These predicates encode that rule.
        let is_unmapped =
            |path: &str| request_type_for_path(path) == "other" && !is_models_endpoint(path);

        // Stray probes / scans -> unmapped -> rejected instead of forwarded.
        assert!(is_unmapped("/props"));
        assert!(is_unmapped("/health"));
        assert!(is_unmapped("/favicon.ico"));
        assert!(is_unmapped("/"));

        // Model-discovery endpoints stay allowed (served without a model).
        assert!(!is_unmapped("/v1/models"));
        assert!(!is_unmapped("/v1/models/gpt-4o"));

        // Recognized OpenAI endpoints are never treated as unmapped.
        assert!(!is_unmapped("/v1/chat/completions"));
        assert!(!is_unmapped("/v1/embeddings"));
        assert!(!is_unmapped("/v1/rerank"));
        assert!(!is_unmapped("/v1/moderations"));
    }

    #[test]
    fn model_info_paths_are_recognized_bare_and_under_v1() {
        use super::{is_model_info_endpoint, is_models_endpoint};
        for path in ["/model/info", "/v1/model/info"] {
            assert!(is_model_info_endpoint(path), "{path}");
            // Also exempt from the unmapped-path rejection, or the route would
            // 404 before its handler ran.
            assert!(is_models_endpoint(path), "{path}");
        }
        assert!(!is_model_info_endpoint("/model/info/extra"));
        assert!(!is_model_info_endpoint("/v1/models"));
    }

    #[test]
    fn stream_usage_is_requested_only_where_upstreams_honour_it() {
        assert!(is_stream_usage_path("/v1/chat/completions"));
        assert!(is_stream_usage_path("/v1/completions"));
        assert!(!is_stream_usage_path("/v1/embeddings"));
        assert!(!is_stream_usage_path("/v1/audio/transcriptions"));
    }
}
