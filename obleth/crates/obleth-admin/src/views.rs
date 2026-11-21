//! Secret-redacted response views of models, endpoints and MCP servers.

use super::*;

/// A registered model route as returned by the Management API. Identical to
/// the stored route except the upstream `api_key` is never returned.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelRouteView {
    pub id: Uuid,
    /// Name clients pass in `model` (e.g. `qwen3-vl-32b-instruct`). Kept free
    /// of deployment detail — the serving format belongs in `quantization`,
    /// not in the name — so re-quantizing does not break every caller.
    pub model_name: String,
    /// Additional client-facing names that resolve to this same route. Exists
    /// so a name can be cleaned up without breaking pinned clients: register
    /// the old `…-fp8` spelling as an alias and it keeps working, while only
    /// `model_name` is advertised by the discovery endpoints.
    pub aliases: Vec<String>,
    /// Opt-in names for this same route with extra boons on (added to the
    /// model's own). A variant shares the model's backend, capacity pool and
    /// prices; unlike an alias it is listed by the discovery endpoints.
    pub variants: Vec<ModelVariant>,
    /// Active, deprecated or retired, with the replacement, retirement date
    /// and note callers are shown. Set with `PUT /api/v1/models/{id}/status`.
    pub lifecycle: ModelLifecycle,
    /// The status in force now: a deprecated model past its retirement date
    /// reads `retired` here while `lifecycle.status` still says `deprecated`.
    pub effective_status: ModelStatus,
    /// How the model takes reasoning effort: pass-through, mapped (each of
    /// none/low/medium/high translated to fields its server understands), or
    /// unsupported. Set with `PUT /api/v1/models/{id}/reasoning`.
    pub reasoning: ModelReasoning,
    /// Whether `/v1/models` and `/model/info` advertise the model. An
    /// unlisted model is still served by name. Set with
    /// `PUT /api/v1/models/{id}/listing`.
    pub listed: bool,
    /// Top-level request body fields refused with 400 before dispatch,
    /// because the model's backend cannot handle them. Set with
    /// `PUT /api/v1/models/{id}/rejected-request-fields`.
    pub rejected_request_fields: Vec<String>,
    /// The model's own session affinity setting; `{}` follows the gateway.
    /// Set with `PUT /api/v1/models/{id}/session-affinity`.
    pub session_affinity: ModelSessionAffinity,
    /// Human-facing summary for operators and dashboards.
    pub description: String,
    /// Value sent to the upstream in the `model` field.
    pub upstream_model: String,
    /// Base URL including `/v1` suffix when required.
    pub api_base: String,
    /// Whether an upstream API key is stored. The key itself is write-only.
    pub api_key_set: bool,
    /// Names of the headers added to every upstream request. The values are
    /// write-only, like `api_key`, because an operator may put a credential
    /// in one.
    pub upstream_header_names: Vec<String>,
    /// Modality from the fixed `MODEL_TYPES` vocabulary. Determines which
    /// OpenAI endpoint this model serves (`chat`, `embedding`,
    /// `audio_transcription`, `audio_speech`, `image`, `video`, or `search`
    /// for a web search tool on `/v1/search`). Defaults to `chat`.
    pub model_type: String,
    /// Weight/activation format this deployment serves, from the fixed
    /// `QUANTIZATIONS` vocabulary. Descriptive only — it never affects
    /// routing; it is reported so clients can tell a `fp8` deployment from a
    /// `bf16` one without reading it out of the model's name.
    pub quantization: String,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    /// Per-generated-image cost in USD (`image` models).
    pub cost_per_image: f64,
    /// Per-second-of-audio cost in USD (`audio_transcription` models).
    pub cost_per_audio_second: f64,
    /// Per-input-character cost in USD (`audio_speech` models).
    pub cost_per_character: f64,
    /// Per-created-job cost in USD (`video` models), charged once when the
    /// create call succeeds.
    pub cost_per_video: f64,
    pub context_window: i64,
    /// Multiplier applied to tenant weight at admission when this model is used.
    pub admission_weight: i64,
    /// Optional per-model in-flight cap. `None` means only the global scheduler
    /// cap and tenant/group fairshare limits apply.
    pub max_in_flight: Option<i64>,
    /// How `max_in_flight` is decided. `static` (default) keeps the
    /// operator-set value; `tuned` means it was found by the auto-tune ramp
    /// probe against the upstream.
    pub capacity_mode: String,
    /// When the tuned `max_in_flight` was last written by auto-tune. `None`
    /// until the model has been tuned.
    pub capacity_tuned_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `discovered` mode: where serving replicas are counted, `endpoints`
    /// (the model's enabled, healthy endpoints) or `kubernetes` (the ready
    /// endpoints of a Service).
    pub capacity_source: String,
    /// `kubernetes` source: namespace of the Service. `None` looks it up in
    /// the gateway's `OBLETH_CAPACITY_DISCOVERY_NAMESPACES`, in order.
    pub capacity_namespace: Option<String>,
    /// `kubernetes` source: name of the Service. `None` uses the gateway's
    /// `OBLETH_CAPACITY_DEFAULT_SERVICE` template.
    pub capacity_service: Option<String>,
    /// Requests one serving replica takes. `None` only for the `endpoints`
    /// source when every endpoint sets its own `max_in_flight`, or outside
    /// the `discovered` mode.
    pub per_replica_max_in_flight: Option<i64>,
    /// Multiplier on the derived pool size; `1.0` is exactly the ready
    /// capacity.
    pub capacity_headroom: f64,
    pub supports_function_calling: bool,
    pub supports_system_messages: bool,
    pub supports_response_schema: bool,
    pub supports_tool_choice: bool,
    /// Native image-input capability. When false, the gateway's vision boon can
    /// relay images to a designated vision model and inject text descriptions
    /// before forwarding the request to this model.
    pub supports_vision: bool,
    pub enabled: bool,
    /// When true, identical requests to this model are served from the response
    /// cache (exact-match on tenant + model + request body) instead of the
    /// upstream. Entries are never shared across tenants.
    pub cache_enabled: bool,
    /// Time-to-live for cached responses, in seconds. `0` disables caching:
    /// nothing is stored (never "store without expiry").
    pub cache_ttl_secs: i64,
    /// Routing tags from the fixed `MODEL_TAGS` vocabulary. The `auto` router
    /// prefers models whose tags match the request's classified intent.
    pub tags: Vec<String>,
    /// Gateway boons enabled for this model from the fixed `MODEL_BOONS`
    /// vocabulary. Boons grant capabilities the model lacks natively (e.g. the
    /// `vision` boon relays images to a describer). Empty by default.
    pub boons: Vec<String>,
    /// Registered MCP servers whose tools this model may use. Distinct from
    /// capabilities: a capability is what the model can do natively (e.g.
    /// function calling); a tool server is something the gateway grants access
    /// to. When non-empty, plain chat requests get the servers' tools injected
    /// and the gateway runs the tool loop itself. Empty by default (off).
    pub tool_servers: Vec<String>,
    /// Per-request upstream timeout in seconds. `None` falls back to the global
    /// `OBLETH_UPSTREAM_TIMEOUT_SECS` default.
    pub request_timeout_secs: Option<i64>,
    /// Extra attempts against the same endpoint on retryable failures (network
    /// errors, timeouts, 408/429/5xx). `0` disables retries.
    pub max_retries: i64,
    /// Base delay in milliseconds for exponential backoff between retries.
    pub retry_backoff_ms: i64,
    /// How obleth chooses among this model's registered endpoints: `failover`
    /// (priority order) or `load_balance` (weighted).
    pub endpoint_selection_mode: String,
    /// When true, a terminal 502/504 against this model triggers read-only
    /// upstream diagnostics (DNS resolve + TCP connect) recorded as a trace
    /// span. Opt-in; off by default. Diagnose-only — never changes routing.
    pub debug_diagnostics: bool,
    /// Declared saturation for energy accounting: how many concurrent
    /// sequences of this model saturate one node (instances per node x
    /// sequences per instance). Each request is charged
    /// `node_watts / energy_slots_per_node` for its serving time.
    /// `0` (default) disables energy accounting for this model.
    pub energy_slots_per_node: i64,
    /// Per-model multiplier on the `auto` router's final score. `1.0` is
    /// neutral; above 1.0 prefers the model, below 1.0 de-prioritizes it.
    pub route_bias: f64,
    /// Whether the `auto` router may select this model. `false` removes it from
    /// auto's candidate pool while leaving it addressable by name — the
    /// distinction from `enabled = false`, which removes it everywhere.
    pub auto_eligible: bool,
    /// Which small model writes this model's speculation drafts. Empty falls
    /// back to the fleet default in the boon settings.
    pub draft_model: String,
    /// Direct (non-gateway) URL of a deployment of THIS model whose backend
    /// supports `prompt_logprobs`; it scores every draft token. Empty means
    /// this model cannot speculate.
    pub verify_api_base: String,
    /// The model name that scoring backend serves, when it differs from this
    /// model's own `upstream_model` (a canary serving its own name). Empty =
    /// same as `upstream_model`.
    pub verify_upstream_model: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

fn secret_set(s: &Option<String>) -> bool {
    s.as_ref().is_some_and(|v| !v.is_empty())
}

pub(crate) fn views<T, V: From<T>>(items: Vec<T>) -> Vec<V> {
    items.into_iter().map(V::from).collect()
}

impl From<ModelRoute> for ModelRouteView {
    fn from(m: ModelRoute) -> Self {
        let ModelRoute {
            id,
            model_name,
            aliases,
            variants,
            lifecycle,
            reasoning,
            listed,
            rejected_request_fields,
            session_affinity,
            description,
            upstream_model,
            api_base,
            api_key,
            upstream_headers,
            model_type,
            quantization,
            input_cost_per_token,
            output_cost_per_token,
            cost_per_image,
            cost_per_audio_second,
            cost_per_character,
            cost_per_video,
            context_window,
            admission_weight,
            max_in_flight,
            capacity_mode,
            capacity_tuned_at,
            capacity_source,
            capacity_namespace,
            capacity_service,
            per_replica_max_in_flight,
            capacity_headroom,
            supports_function_calling,
            supports_system_messages,
            supports_response_schema,
            supports_tool_choice,
            supports_vision,
            enabled,
            cache_enabled,
            cache_ttl_secs,
            tags,
            boons,
            tool_servers,
            request_timeout_secs,
            max_retries,
            retry_backoff_ms,
            endpoint_selection_mode,
            debug_diagnostics,
            energy_slots_per_node,
            route_bias,
            auto_eligible,
            draft_model,
            verify_api_base,
            verify_upstream_model,
            created_at,
            updated_at,
        } = m;
        ModelRouteView {
            id,
            model_name,
            aliases,
            variants,
            effective_status: lifecycle.status_at(chrono::Utc::now()),
            lifecycle,
            reasoning,
            listed,
            rejected_request_fields,
            session_affinity,
            description,
            upstream_model,
            api_base,
            api_key_set: secret_set(&api_key),
            upstream_header_names: obleth_config::upstream_header_names(&upstream_headers),
            model_type,
            quantization,
            input_cost_per_token,
            output_cost_per_token,
            cost_per_image,
            cost_per_audio_second,
            cost_per_character,
            cost_per_video,
            context_window,
            admission_weight,
            max_in_flight,
            capacity_mode,
            capacity_tuned_at,
            capacity_source,
            capacity_namespace,
            capacity_service,
            per_replica_max_in_flight,
            capacity_headroom,
            supports_function_calling,
            supports_system_messages,
            supports_response_schema,
            supports_tool_choice,
            supports_vision,
            enabled,
            cache_enabled,
            cache_ttl_secs,
            tags,
            boons,
            tool_servers,
            request_timeout_secs,
            max_retries,
            retry_backoff_ms,
            endpoint_selection_mode,
            debug_diagnostics,
            energy_slots_per_node,
            route_bias,
            auto_eligible,
            draft_model,
            verify_api_base,
            verify_upstream_model,
            created_at,
            updated_at,
        }
    }
}

/// A model's upstream endpoint as returned by the Management API. The
/// endpoint `api_key` is never returned.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelEndpointView {
    pub id: Uuid,
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub name: String,
    pub api_base: String,
    /// Whether an endpoint API key is stored. The key itself is write-only.
    pub api_key_set: bool,
    pub priority: i64,
    pub weight: i64,
    pub enabled: bool,
    /// Requests this endpoint takes at once, counted by a `discovered` model
    /// using the `endpoints` source. `None` uses the model's
    /// `per_replica_max_in_flight`.
    pub max_in_flight: Option<i64>,
    pub health_status: String,
    pub consecutive_failures: i64,
    pub alert_state: String,
    pub last_checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_latency_ms: Option<i64>,
    pub last_http_status: Option<i64>,
    pub last_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<ModelEndpoint> for ModelEndpointView {
    fn from(e: ModelEndpoint) -> Self {
        let ModelEndpoint {
            id,
            model_id,
            name,
            api_base,
            api_key,
            priority,
            weight,
            enabled,
            max_in_flight,
            health_status,
            consecutive_failures,
            alert_state,
            last_checked_at,
            last_latency_ms,
            last_http_status,
            last_message,
            created_at,
            updated_at,
        } = e;
        ModelEndpointView {
            id,
            model_id,
            name,
            api_base,
            api_key_set: secret_set(&api_key),
            priority,
            weight,
            enabled,
            max_in_flight,
            health_status,
            consecutive_failures,
            alert_state,
            last_checked_at,
            last_latency_ms,
            last_http_status,
            last_message,
            created_at,
            updated_at,
        }
    }
}

/// A registered MCP server as returned by the Management API. The upstream
/// `auth_header` is never returned.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct McpServerView {
    pub id: Uuid,
    pub name: String,
    pub upstream_url: String,
    /// Whether an upstream Authorization header is stored. The value itself is
    /// write-only.
    pub auth_header_set: bool,
    pub enabled: bool,
    /// The tenants whose keys may call the server through `/mcp/{name}`.
    pub allowed_tenants: Vec<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<McpServer> for McpServerView {
    fn from(s: McpServer) -> Self {
        let McpServer {
            id,
            name,
            upstream_url,
            auth_header,
            enabled,
            allowed_tenants,
            created_at,
            updated_at,
        } = s;
        McpServerView {
            id,
            name,
            upstream_url,
            auth_header_set: secret_set(&auth_header),
            enabled,
            allowed_tenants,
            created_at,
            updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::fixture_model_route;

    #[test]
    fn model_route_view_lists_variants() {
        let mut route = fixture_model_route("glm-5-3");
        route.variants = vec![obleth_config::ModelVariant {
            name: "glm-5-3-spec".into(),
            description: "drafted, then verified".into(),
            boons: vec!["speculation".into()],
        }];
        let v = serde_json::to_value(ModelRouteView::from(route)).unwrap();
        assert_eq!(
            v["variants"],
            serde_json::json!([{
                "name": "glm-5-3-spec",
                "description": "drafted, then verified",
                "boons": ["speculation"],
            }])
        );
        let v = serde_json::to_value(ModelRouteView::from(fixture_model_route("m"))).unwrap();
        assert_eq!(v["variants"], serde_json::json!([]));
    }

    #[test]
    fn model_route_view_redacts_api_key() {
        let mut route = fixture_model_route("m");
        route.api_key = Some("sk-upstream-secret".into());
        let v = serde_json::to_value(ModelRouteView::from(route.clone())).unwrap();
        assert_eq!(v["api_key_set"], serde_json::json!(true));
        assert!(v.get("api_key").is_none(), "view leaked api_key: {v}");
        assert!(!v.to_string().contains("sk-upstream-secret"));
        assert_eq!(v["model_name"], "m");

        route.api_key = None;
        let v = serde_json::to_value(ModelRouteView::from(route.clone())).unwrap();
        assert_eq!(v["api_key_set"], serde_json::json!(false));
        // An empty stored key is "no key" — the proxy sends no auth for it.
        route.api_key = Some(String::new());
        let v = serde_json::to_value(ModelRouteView::from(route)).unwrap();
        assert_eq!(v["api_key_set"], serde_json::json!(false));
    }

    #[test]
    fn model_route_view_lists_upstream_header_names_but_never_values() {
        let mut route = fixture_model_route("m");
        route.upstream_headers = [
            ("x-routing-hint".to_string(), "sticky".to_string()),
            (
                "x-upstream-token".to_string(),
                "tok-upstream-secret".to_string(),
            ),
        ]
        .into();
        let v = serde_json::to_value(ModelRouteView::from(route)).unwrap();
        assert_eq!(
            v["upstream_header_names"],
            serde_json::json!(["x-routing-hint", "x-upstream-token"])
        );
        assert!(
            v.get("upstream_headers").is_none(),
            "view leaked values: {v}"
        );
        assert!(!v.to_string().contains("tok-upstream-secret"));
        assert!(!v.to_string().contains("sticky"));
    }

    #[test]
    fn model_endpoint_view_redacts_api_key() {
        let now = chrono::Utc::now();
        let ep = ModelEndpoint {
            id: Uuid::new_v4(),
            model_id: Uuid::new_v4(),
            name: "primary".into(),
            api_base: "http://upstream.invalid/v1".into(),
            api_key: Some("sk-endpoint-secret".into()),
            priority: 0,
            weight: 100,
            enabled: true,
            max_in_flight: None,
            health_status: "unknown".into(),
            consecutive_failures: 0,
            alert_state: "ok".into(),
            last_checked_at: None,
            last_latency_ms: None,
            last_http_status: None,
            last_message: None,
            created_at: now,
            updated_at: now,
        };
        let v = serde_json::to_value(ModelEndpointView::from(ep.clone())).unwrap();
        assert_eq!(v["api_key_set"], serde_json::json!(true));
        assert!(v.get("api_key").is_none(), "view leaked api_key: {v}");
        assert!(!v.to_string().contains("sk-endpoint-secret"));
        assert_eq!(v["name"], "primary");

        let v = serde_json::to_value(ModelEndpointView::from(ModelEndpoint {
            api_key: None,
            ..ep
        }))
        .unwrap();
        assert_eq!(v["api_key_set"], serde_json::json!(false));
    }

    #[test]
    fn mcp_server_view_redacts_auth_header() {
        let now = chrono::Utc::now();
        let server = McpServer {
            id: Uuid::new_v4(),
            name: "files".into(),
            upstream_url: "http://mcp.invalid/mcp".into(),
            auth_header: Some("Bearer mcp-secret".into()),
            enabled: true,
            allowed_tenants: vec![],
            created_at: now,
            updated_at: now,
        };
        let v = serde_json::to_value(McpServerView::from(server.clone())).unwrap();
        assert_eq!(v["auth_header_set"], serde_json::json!(true));
        assert!(
            v.get("auth_header").is_none(),
            "view leaked auth_header: {v}"
        );
        assert!(!v.to_string().contains("mcp-secret"));
        assert_eq!(v["name"], "files");

        let v = serde_json::to_value(McpServerView::from(McpServer {
            auth_header: None,
            ..server
        }))
        .unwrap();
        assert_eq!(v["auth_header_set"], serde_json::json!(false));
    }
}
