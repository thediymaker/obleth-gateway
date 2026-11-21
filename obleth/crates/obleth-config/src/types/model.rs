//! Model routes, endpoints and health, the model vocabularies and their
//! normalisers, lifecycle and per-model session affinity.

use super::*;

/// Registered model route. Client-facing `model_name` maps to an upstream
/// OpenAI-compatible endpoint (an inference gateway, a model server, or an external API).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelRoute {
    pub id: Uuid,
    /// Name clients pass in `model` (e.g. `qwen3-vl-32b-instruct`). Kept free
    /// of deployment detail — the serving format belongs in `quantization`,
    /// not in the name — so re-quantizing does not break every caller.
    pub model_name: String,
    /// Additional client-facing names that resolve to this same route. Exists
    /// so a name can be cleaned up without breaking pinned clients: register
    /// the old `…-fp8` spelling as an alias and it keeps working, while only
    /// `model_name` is advertised by the discovery endpoints.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Opt-in names for this same route with extra boons on (see
    /// [`ModelVariant`]). Unlike an alias, a variant changes what the request
    /// gets, so it is listed by the discovery endpoints. `#[serde(default)]`
    /// keeps payloads written before variants existed readable as "none".
    #[serde(default)]
    pub variants: Vec<ModelVariant>,
    /// Staged, active, deprecated or retired, with the replacement and
    /// retirement date (see [`ModelLifecycle`]). `#[serde(default)]` reads older payloads
    /// as active.
    #[serde(default)]
    pub lifecycle: ModelLifecycle,
    /// How the model takes reasoning effort (see [`ModelReasoning`]).
    /// `#[serde(default)]` reads older payloads as pass-through.
    #[serde(default)]
    pub reasoning: ModelReasoning,
    /// Whether `/v1/models` and `/model/info` advertise this model. An
    /// unlisted model is still served by name; it just isn't offered, e.g.
    /// when a model family stands for it. `#[serde(default)]` reads older
    /// payloads as listed.
    #[serde(default = "default_true")]
    pub listed: bool,
    /// Top-level request body fields this model's backend cannot handle. A
    /// request carrying any of them (or carrying one inside `extra_body`) is
    /// refused with 400 before it is sent. Case-sensitive JSON keys; empty by
    /// default. `#[serde(default)]` reads older payloads as "none".
    #[serde(default)]
    pub rejected_request_fields: Vec<String>,
    /// Session affinity for this model's pool: on or off whatever the
    /// gateway's switch says, and values of its own. `{}` (the default)
    /// follows the gateway. `#[serde(default)]` reads older payloads so.
    #[serde(default)]
    pub session_affinity: ModelSessionAffinity,
    /// Human-facing summary for operators and dashboards.
    pub description: String,
    /// Value sent to the upstream in the `model` field.
    pub upstream_model: String,
    /// Base URL including `/v1` suffix when required.
    pub api_base: String,
    /// Optional bearer/api key for the upstream (stored encrypted-at-rest in prod).
    pub api_key: Option<String>,
    /// Extra headers sent on every request to this model's upstream, after
    /// the client's forwarded headers so the operator's value wins. Values
    /// are stored encrypted at rest and never returned by the Management API.
    #[serde(default)]
    #[schema(value_type = std::collections::BTreeMap<String, String>)]
    pub upstream_headers: UpstreamHeaders,
    /// Modality from the fixed [`MODEL_TYPES`] vocabulary. Determines which
    /// OpenAI endpoint this model serves (`chat`, `embedding`,
    /// `audio_transcription`, `audio_speech`, `image`, `video`). Defaults to
    /// `chat`.
    #[serde(default = "default_model_type")]
    pub model_type: String,
    /// Weight/activation format this deployment serves, from the fixed
    /// [`QUANTIZATIONS`] vocabulary. Descriptive only — it never affects
    /// routing; it is reported so clients can tell a `fp8` deployment from a
    /// `bf16` one without reading it out of the model's name.
    #[serde(default = "default_quantization")]
    pub quantization: String,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    /// Per-generated-image cost in USD (`image` models).
    #[serde(default)]
    pub cost_per_image: f64,
    /// Per-second-of-audio cost in USD (`audio_transcription` models).
    #[serde(default)]
    pub cost_per_audio_second: f64,
    /// Per-input-character cost in USD (`audio_speech` models).
    #[serde(default)]
    pub cost_per_character: f64,
    /// Per-created-job cost in USD (`video` models), charged once when the
    /// create call succeeds.
    #[serde(default)]
    pub cost_per_video: f64,
    pub context_window: i64,
    /// Multiplier applied to tenant weight at admission when this model is used.
    pub admission_weight: i64,
    /// Optional per-model in-flight cap. `None` means only the global scheduler
    /// cap and tenant/group fairshare limits apply.
    pub max_in_flight: Option<i64>,
    /// How `max_in_flight` is decided. `static` (default) keeps the
    /// operator-set value; `tuned` means it was found by the auto-tune ramp
    /// probe against the upstream. Stored as text for forward compatibility.
    #[serde(default = "default_capacity_mode")]
    pub capacity_mode: String,
    /// When the tuned `max_in_flight` was last written by auto-tune. `None`
    /// until the model has been tuned.
    #[serde(default)]
    pub capacity_tuned_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `discovered` mode: where serving replicas are counted, from the fixed
    /// [`CAPACITY_SOURCES`] vocabulary. `endpoints` (default) counts the
    /// model's own enabled, healthy endpoints; `kubernetes` counts the ready
    /// endpoints of the Service `capacity_service`.
    #[serde(default = "default_capacity_source")]
    pub capacity_source: String,
    /// `kubernetes` source: namespace of the Service. `None` looks it up in
    /// `OBLETH_CAPACITY_DISCOVERY_NAMESPACES` in order; the first namespace
    /// that has it wins.
    #[serde(default)]
    pub capacity_namespace: Option<String>,
    /// `kubernetes` source: name of the Service whose ready endpoints are the
    /// serving replicas. `None` uses the gateway's
    /// `OBLETH_CAPACITY_DEFAULT_SERVICE` template.
    #[serde(default)]
    pub capacity_service: Option<String>,
    /// `discovered` mode: concurrent requests one serving replica takes.
    /// Required on the `kubernetes` source, and on `endpoints` unless every
    /// endpoint sets its own `max_in_flight`.
    #[serde(default)]
    pub per_replica_max_in_flight: Option<i64>,
    /// `discovered` mode: multiplier on the derived pool size. `1.0` admits
    /// exactly the ready capacity; above it lets requests queue at the
    /// backend, which an autoscaler scaling on queue depth needs to see.
    #[serde(default = "default_capacity_headroom")]
    pub capacity_headroom: f64,
    pub supports_function_calling: bool,
    pub supports_system_messages: bool,
    pub supports_response_schema: bool,
    pub supports_tool_choice: bool,
    /// Native image-input capability. When false, the gateway's vision boon can
    /// relay images to a designated vision model and inject text descriptions
    /// before forwarding the request to this model.
    #[serde(default)]
    pub supports_vision: bool,
    pub enabled: bool,
    /// When true, identical requests to this model are served from the response
    /// cache (exact-match on tenant + model + request body) instead of the
    /// upstream. Entries are never shared across tenants.
    pub cache_enabled: bool,
    /// Time-to-live for cached responses, in seconds. `0` disables caching:
    /// nothing is stored (never "store without expiry").
    pub cache_ttl_secs: i64,
    /// Routing tags from the fixed [`MODEL_TAGS`] vocabulary. The `auto` router
    /// prefers models whose tags match the request's classified intent.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Gateway boons enabled for this model from the fixed [`MODEL_BOONS`]
    /// vocabulary. Boons grant capabilities the model lacks natively (e.g. the
    /// `vision` boon relays images to a describer). Empty by default.
    #[serde(default)]
    pub boons: Vec<String>,
    /// Registered MCP servers whose tools this model may use. Distinct from
    /// capabilities: a capability is what the model can do natively (e.g.
    /// function calling); a tool server is something the gateway grants access
    /// to. When non-empty, plain chat requests get the servers' tools injected
    /// and the gateway runs the tool loop itself. Empty by default (off).
    #[serde(default)]
    pub tool_servers: Vec<String>,
    /// Per-request upstream timeout in seconds. `None` falls back to the global
    /// `OBLETH_UPSTREAM_TIMEOUT_SECS` default.
    #[serde(default)]
    pub request_timeout_secs: Option<i64>,
    /// Extra attempts against the same endpoint on retryable failures (network
    /// errors, timeouts, 408/429/5xx). `0` disables retries.
    #[serde(default)]
    pub max_retries: i64,
    /// Base delay in milliseconds for exponential backoff between retries.
    #[serde(default = "default_retry_backoff_ms")]
    pub retry_backoff_ms: i64,
    /// How obleth chooses among this model's registered endpoints: `failover`
    /// (priority order) or `load_balance` (weighted).
    #[serde(default = "default_endpoint_selection_mode")]
    pub endpoint_selection_mode: String,
    /// When true, a terminal 502/504 against this model triggers read-only
    /// upstream diagnostics (DNS resolve + TCP connect) recorded as a trace
    /// span. Opt-in; off by default. Diagnose-only — never changes routing.
    #[serde(default)]
    pub debug_diagnostics: bool,
    /// Declared saturation for energy accounting: how many concurrent
    /// sequences of this model saturate one node (instances per node x
    /// sequences per instance). Each request is charged
    /// `node_watts / energy_slots_per_node` for its serving time.
    /// `0` (default) disables energy accounting for this model.
    #[serde(default)]
    pub energy_slots_per_node: i64,
    /// Per-model multiplier on the `auto` router's final score. `1.0` is
    /// neutral; above 1.0 prefers the model, below 1.0 de-prioritizes it.
    /// `#[serde(default = "default_route_bias")]` keeps older payloads
    /// deserializable as neutral rather than the score-zeroing `0.0`.
    #[serde(default = "default_route_bias")]
    pub route_bias: f64,
    /// Whether the `auto` router may select this model. `false` removes it from
    /// auto's candidate pool while leaving it addressable by name — the
    /// distinction from `enabled = false`, which removes it everywhere.
    /// Defaults to `true`, not `bool::default()`, so an older payload never
    /// reads as an exclusion.
    #[serde(default = "default_auto_eligible")]
    pub auto_eligible: bool,
    /// Which small model writes this model's speculation drafts. Empty falls
    /// back to the fleet default in the boon settings.
    #[serde(default)]
    pub draft_model: String,
    /// Direct (non-gateway) URL of a deployment of THIS model whose backend
    /// supports `prompt_logprobs`; it scores every draft token. Empty means
    /// this model cannot speculate.
    #[serde(default)]
    pub verify_api_base: String,
    /// The model name that scoring backend serves, when it differs from this
    /// model's own `upstream_model` (a canary serving its own name). Empty =
    /// same as `upstream_model`.
    #[serde(default)]
    pub verify_upstream_model: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl ModelRoute {
    /// The model was granted `boon`.
    pub fn has_boon(&self, boon: Boon) -> bool {
        boon.granted(&self.boons)
    }

    /// Every client-facing name this route answers to: `model_name`, its
    /// aliases, then its variants. The same set
    /// [`ResolvedModel::addressable_names`] publishes as resolver keys.
    pub fn addressable_names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.model_name.as_str())
            .chain(self.aliases.iter().map(String::as_str))
            .chain(self.variants.iter().map(|v| v.name.as_str()))
    }
}

/// Persisted model-health status. Stored as snake_case text in Postgres so new
/// UI/API readers can remain forward-compatible with older rows.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelHealthSummary {
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub model_name: String,
    pub checks_enabled: bool,
    pub alerts_enabled: bool,
    pub check_interval_secs: i64,
    pub failure_threshold: i64,
    pub maintenance_until: Option<chrono::DateTime<chrono::Utc>>,
    pub maintenance_note: Option<String>,
    pub status: String,
    pub consecutive_failures: i64,
    pub alert_state: String,
    pub next_check_at: chrono::DateTime<chrono::Utc>,
    pub last_checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_latency_ms: Option<i64>,
    pub last_http_status: Option<i64>,
    pub last_message: Option<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelHealthCheck {
    pub id: i64,
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub checked_at: chrono::DateTime<chrono::Utc>,
    pub trigger: String,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub http_status: Option<i64>,
    pub message: Option<String>,
    pub response_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelHealthDetail {
    pub summary: ModelHealthSummary,
    pub checks: Vec<ModelHealthCheck>,
}

/// Hot-path view of a model route for the data plane cache.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedModel {
    pub model_name: String,
    /// Extra names this route answers to. Carried on the hot-path view so the
    /// discovery endpoints can report them, and so a resolver warm-up knows
    /// every key to publish; request matching itself is a direct key lookup on
    /// whichever name the client sent. `#[serde(default)]` keeps older cached
    /// payloads deserializable as "no aliases".
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Extra names that resolve to this route with more boons on (see
    /// [`ModelVariant`]). Each is published as its own resolver key pointing
    /// at this same route; the proxy adds the variant's boons per request
    /// ([`ResolvedModel::with_variant`]). `#[serde(default)]` keeps older
    /// cached payloads deserializable as "no variants".
    #[serde(default)]
    pub variants: Vec<ModelVariant>,
    /// See [`ModelRoute::lifecycle`]. Read per request: the proxy refuses a
    /// retired model and marks a deprecated one's responses. `#[serde(default)]`
    /// keeps older cached payloads deserializable as "active".
    #[serde(default)]
    pub lifecycle: ModelLifecycle,
    /// See [`ModelRoute::reasoning`]. Read per request to translate the
    /// caller's effort. `#[serde(default)]` keeps older cached payloads
    /// deserializable as "pass-through".
    #[serde(default)]
    pub reasoning: ModelReasoning,
    /// See [`ModelRoute::listed`]. `#[serde(default)]` keeps older cached
    /// payloads deserializable as "listed".
    #[serde(default = "default_true")]
    pub listed: bool,
    /// See [`ModelRoute::rejected_request_fields`]. Read per request.
    /// `#[serde(default)]` keeps older cached payloads deserializable as
    /// "none".
    #[serde(default)]
    pub rejected_request_fields: Vec<String>,
    /// See [`ModelRoute::session_affinity`]. Read per admission.
    /// `#[serde(default)]` keeps older cached payloads deserializable as
    /// "follows the gateway".
    #[serde(default)]
    pub session_affinity: ModelSessionAffinity,
    /// See [`ModelRoute::description`]. Carried so the discovery endpoints
    /// can say what a model is for. `#[serde(default)]` keeps older cached
    /// payloads deserializable as "no description".
    #[serde(default)]
    pub description: String,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    /// Operator-configured headers for every upstream request (see
    /// [`ModelRoute::upstream_headers`]). `#[serde(default)]` keeps older
    /// cached payloads deserializable as "none".
    #[serde(default)]
    pub upstream_headers: UpstreamHeaders,
    /// Modality from the fixed [`MODEL_TYPES`] vocabulary. `#[serde(default)]`
    /// keeps older cached payloads (without this field) deserializable as
    /// `chat`.
    #[serde(default = "default_model_type")]
    pub model_type: String,
    /// Serving format from the fixed [`QUANTIZATIONS`] vocabulary, reported by
    /// the discovery endpoints. `#[serde(default …)]` keeps older cached
    /// payloads deserializable as `unknown`.
    #[serde(default = "default_quantization")]
    pub quantization: String,
    pub admission_weight: i64,
    pub max_in_flight: Option<usize>,
    /// How the pool size is decided (see [`ModelRoute::capacity_mode`]). The
    /// data plane only acts on `discovered`. `#[serde(default …)]` keeps
    /// older cached payloads deserializable as `static`.
    #[serde(default = "default_capacity_mode")]
    pub capacity_mode: String,
    /// `discovered` mode inputs (see [`ModelRoute::capacity_source`] and
    /// its siblings). The defaults keep older cached payloads readable.
    #[serde(default = "default_capacity_source")]
    pub capacity_source: String,
    #[serde(default)]
    pub capacity_namespace: Option<String>,
    #[serde(default)]
    pub capacity_service: Option<String>,
    #[serde(default)]
    pub per_replica_max_in_flight: Option<usize>,
    #[serde(default = "default_capacity_headroom")]
    pub capacity_headroom: f64,
    pub enabled: bool,
    pub cache_enabled: bool,
    pub cache_ttl_secs: i64,
    /// Cost per input/output token, used for cumulative USD budget accounting.
    #[serde(default)]
    pub input_cost_per_token: f64,
    #[serde(default)]
    pub output_cost_per_token: f64,
    /// Per-unit costs for non-chat modalities (USD).
    #[serde(default)]
    pub cost_per_image: f64,
    #[serde(default)]
    pub cost_per_audio_second: f64,
    #[serde(default)]
    pub cost_per_character: f64,
    #[serde(default)]
    pub cost_per_video: f64,
    /// Maximum context window in tokens. Used by the `auto` router to filter
    /// out models that cannot fit the request. `#[serde(default)]` keeps older
    /// cached payloads (without this field) deserializable.
    #[serde(default)]
    pub context_window: i64,
    #[serde(default)]
    pub supports_function_calling: bool,
    #[serde(default)]
    pub supports_system_messages: bool,
    #[serde(default)]
    pub supports_response_schema: bool,
    #[serde(default)]
    pub supports_tool_choice: bool,
    /// Native image-input capability. When false, the gateway's vision boon can
    /// relay images to a designated vision model. `#[serde(default)]` keeps
    /// older cached payloads (without this field) deserializable as `false`.
    #[serde(default)]
    pub supports_vision: bool,
    /// Routing tags from the fixed [`MODEL_TAGS`] vocabulary.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Declared per-topic strength, parsed from `tag:level` suffixes. This is what
    /// the operator typed; the effective level after cost-rank derivation lives on
    /// `router::Candidate`. `#[serde(default)]` keeps older cached payloads readable.
    #[serde(default)]
    pub declared_levels: Vec<(String, u8)>,
    /// Gateway boons enabled for this model from the fixed [`MODEL_BOONS`]
    /// vocabulary. `#[serde(default)]` keeps older cached payloads readable.
    #[serde(default)]
    pub boons: Vec<String>,
    /// Registered MCP servers whose tools this model may use (gateway tool
    /// loop). `#[serde(default)]` keeps older cached payloads readable.
    #[serde(default)]
    pub tool_servers: Vec<String>,
    /// Knowledge collections this model grounds on. `#[serde(default)]` keeps
    /// Redis payloads cached by an older build deserializable.
    #[serde(default)]
    pub knowledge_collections: Vec<Uuid>,
    /// Per-request upstream timeout in seconds. `None` falls back to the global
    /// default. `#[serde(default)]` keeps older cached payloads readable.
    #[serde(default)]
    pub request_timeout_secs: Option<i64>,
    /// Extra attempts against the same endpoint on retryable failures.
    #[serde(default)]
    pub max_retries: i64,
    /// Base delay (ms) for exponential backoff between retries.
    #[serde(default = "default_retry_backoff_ms")]
    pub retry_backoff_ms: i64,
    /// How to choose among `endpoints`: `failover` or `load_balance`.
    #[serde(default = "default_endpoint_selection_mode")]
    pub endpoint_selection_mode: String,
    #[serde(default)]
    pub debug_diagnostics: bool,
    /// Declared saturation for energy accounting: how many concurrent
    /// sequences of this model saturate one node (instances per node x
    /// sequences per instance). Each request is charged
    /// `node_watts / energy_slots_per_node` for its serving time.
    /// `0` (default) disables energy accounting for this model.
    #[serde(default)]
    pub energy_slots_per_node: i64,
    /// Per-model multiplier on the `auto` router's final score. `1.0` is
    /// neutral; above 1.0 prefers the model, below 1.0 de-prioritizes it.
    /// `#[serde(default = "default_route_bias")]` keeps older cached Redis
    /// payloads deserializable as neutral rather than the score-zeroing `0.0`.
    #[serde(default = "default_route_bias")]
    pub route_bias: f64,
    /// Whether the `auto` router may select this model. `false` removes it from
    /// auto's candidate pool while leaving it addressable by name — the
    /// distinction from `enabled = false`, which removes it everywhere.
    /// Defaults to `true`, not `bool::default()`: a cached payload written
    /// before this field existed must not read as an exclusion.
    #[serde(default = "default_auto_eligible")]
    pub auto_eligible: bool,
    /// Which small model writes this model's speculation drafts. Empty falls
    /// back to the fleet default in the boon settings.
    #[serde(default)]
    pub draft_model: String,
    /// Direct (non-gateway) URL of a deployment of THIS model whose backend
    /// supports `prompt_logprobs`; it scores every draft token. Empty means
    /// this model cannot speculate.
    #[serde(default)]
    pub verify_api_base: String,
    /// The model name that scoring backend serves, when it differs from this
    /// model's own `upstream_model` (a canary serving its own name). Empty =
    /// same as `upstream_model`.
    #[serde(default)]
    pub verify_upstream_model: String,
    /// Upstream endpoints for this model. When empty, the data plane falls back
    /// to the legacy single `api_base`/`api_key` pair above (older cached
    /// payloads and un-migrated rows).
    #[serde(default)]
    pub endpoints: Vec<ResolvedEndpoint>,
}

impl Default for ResolvedModel {
    /// A route with the gateway's defaults for every setting a new model
    /// starts with, serving nothing yet: the base for fixtures and other
    /// literals that set only what they are about.
    fn default() -> Self {
        Self {
            model_name: String::new(),
            aliases: Vec::new(),
            variants: Vec::new(),
            lifecycle: ModelLifecycle::default(),
            reasoning: ModelReasoning::default(),
            listed: true,
            rejected_request_fields: Vec::new(),
            session_affinity: ModelSessionAffinity::default(),
            description: String::new(),
            upstream_model: String::new(),
            api_base: String::new(),
            api_key: None,
            upstream_headers: UpstreamHeaders::new(),
            model_type: DEFAULT_MODEL_TYPE.to_string(),
            quantization: DEFAULT_QUANTIZATION.to_string(),
            admission_weight: 100,
            max_in_flight: None,
            capacity_mode: DEFAULT_CAPACITY_MODE.to_string(),
            capacity_source: DEFAULT_CAPACITY_SOURCE.to_string(),
            capacity_namespace: None,
            capacity_service: None,
            per_replica_max_in_flight: None,
            capacity_headroom: 1.0,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            cost_per_video: 0.0,
            context_window: 0,
            supports_function_calling: false,
            supports_system_messages: true,
            supports_response_schema: false,
            supports_tool_choice: false,
            supports_vision: false,
            tags: Vec::new(),
            declared_levels: Vec::new(),
            boons: Vec::new(),
            tool_servers: Vec::new(),
            knowledge_collections: Vec::new(),
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: DEFAULT_RETRY_BACKOFF_MS,
            endpoint_selection_mode: DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            route_bias: DEFAULT_ROUTE_BIAS,
            auto_eligible: DEFAULT_AUTO_ELIGIBLE,
            draft_model: String::new(),
            verify_api_base: String::new(),
            verify_upstream_model: String::new(),
            endpoints: Vec::new(),
        }
    }
}

impl ResolvedModel {
    /// The model was granted `boon`.
    pub fn has_boon(&self, boon: Boon) -> bool {
        boon.granted(&self.boons)
    }

    /// Every client-facing name that must resolve to this route: the canonical
    /// `model_name` first, then each alias, then each variant. This is the set
    /// of resolver keys a warm-up or a cache invalidation has to cover, so
    /// publishing and eviction stay in step with whatever names the model
    /// currently declares.
    pub fn addressable_names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.model_name.as_str())
            .chain(self.aliases.iter().map(String::as_str))
            .chain(self.variants.iter().map(|v| v.name.as_str()))
    }

    /// The variant of this route called `name`, if there is one. Exact,
    /// case-sensitive match, like every other model name.
    pub fn variant(&self, name: &str) -> Option<&ModelVariant> {
        self.variants.iter().find(|v| v.name == name)
    }

    /// This route as served through its variant `name`: identical in every
    /// field except `boons`, which gains the variant's boons after the model's
    /// own. `None` when `name` is not one of this route's variants.
    ///
    /// Everything that identifies the backend stays the parent's on purpose —
    /// `model_name`, the upstream, endpoints, prices and every capacity field
    /// — so the admission pool, fairshare, discovered capacity and the usage
    /// ledger all see one model, never two pools for one backend.
    pub fn with_variant(&self, name: &str) -> Option<ResolvedModel> {
        let variant = self.variant(name)?;
        let mut served = self.clone();
        served.boons = union_boons(&self.boons, &variant.boons);
        Some(served)
    }
}

/// Hot-path view of one upstream endpoint of a model. Several endpoints of the
/// same model are interchangeable (identical pricing and capabilities); only
/// the wire target (`api_base`/`api_key`) differs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedEndpoint {
    #[serde(default)]
    pub id: String,
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Lower wins in `failover` mode.
    #[serde(default)]
    pub priority: i64,
    /// Relative share in `load_balance` mode.
    #[serde(default)]
    pub weight: i64,
    /// Whether this endpoint is eligible for traffic at all.
    #[serde(default)]
    pub enabled: bool,
    /// Last observed health. Unhealthy endpoints are skipped during selection.
    #[serde(default)]
    pub healthy: bool,
    /// Requests this endpoint takes at once, for a `discovered` model using
    /// the `endpoints` source. `None` uses the model's
    /// `per_replica_max_in_flight`.
    #[serde(default)]
    pub max_in_flight: Option<usize>,
}

/// Persisted upstream endpoint of a model (control-plane/API view). Several
/// endpoints of the same model are interchangeable; obleth fails over or
/// load-balances across them and tracks health per endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelEndpoint {
    pub id: Uuid,
    #[schema(value_type = String)]
    pub model_id: Uuid,
    /// Operator-facing label, unique within the model.
    pub name: String,
    pub api_base: String,
    /// Optional bearer/api key for this endpoint (stored encrypted-at-rest).
    pub api_key: Option<String>,
    /// Lower wins in `failover` mode.
    pub priority: i64,
    /// Relative share in `load_balance` mode.
    pub weight: i64,
    pub enabled: bool,
    /// Requests this endpoint takes at once, counted by a `discovered` model
    /// using the `endpoints` source. `None` uses the model's
    /// `per_replica_max_in_flight`.
    #[serde(default)]
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

/// Fixed vocabulary of model routing tags. Operators tag each model with a
/// subset of these; the `auto` router's classifier maps a request to the
/// best-matching tags and selection prefers models carrying them.
pub const MODEL_TAGS: &[&str] = &[
    "coding",
    "general",
    "reasoning",
    "math",
    "vision",
    "long-context",
    "fast",
    "creative",
    "writing",
];

/// Tag pairs a request should not carry together. "Write" is the verb of both
/// kinds of request, so an intent classifier asked about `writing` says yes to
/// "write a SQL query" nearly as readily as to "write a cover letter". When
/// both clear the threshold, the weaker of the pair is dropped.
pub const EXCLUSIVE_TAG_PAIRS: &[(&str, &str)] = &[("coding", "writing")];

/// True when `tag` is part of the fixed [`MODEL_TAGS`] vocabulary.
pub fn is_valid_tag(tag: &str) -> bool {
    MODEL_TAGS.contains(&tag)
}

/// Highest strength level in the tier ladder.
pub const MAX_TIER_LEVEL: u8 = 3;

/// Split an optional `:1|2|3` strength suffix off a tag and validate the base
/// against [`MODEL_TAGS`]. A bare tag is level 1, so every configuration
/// written before tiering existed keeps its meaning. An unparseable or
/// out-of-range level clamps into the ladder rather than rejecting the tag —
/// a typo should not silently drop a model's topic.
pub fn parse_tag_level(raw: &str) -> Option<(String, u8)> {
    let raw = raw.trim().to_ascii_lowercase();
    let (base, level) = match raw.split_once(':') {
        Some((b, l)) => {
            let parsed = l.trim().parse::<u8>().unwrap_or(1);
            (b.trim().to_string(), parsed.clamp(1, MAX_TIER_LEVEL))
        }
        None => (raw, 1),
    };
    if is_valid_tag(&base) {
        Some((base, level))
    } else {
        None
    }
}

/// Fixed vocabulary of gateway boons. A boon grants a capability a model lacks
/// natively; the data plane applies a model's enabled boons before dispatch.
/// `vision` relays image parts to a configured describer model.
/// `structured_output` enforces `response_format` JSON schemas with gateway-side
/// validation and repair. `compression` reduces the input tokens a model reads
/// (JSON/code compaction, cross-turn dedup, deterministic lossy text compaction) before dispatch.
/// `knowledge` grounds chat requests on admin-curated institutional documents
/// retrieved from an in-process vector index.
/// `image_generation` injects a gateway-executed `generate_image` tool so a chat
/// model can produce images through a registered image model.
/// `speculation` answers with a fast drafter model when the target model itself
/// verifies the draft (one cheap prefill scores every draft token via
/// prompt_logprobs); unverified drafts escalate to the target model.
/// `web_search` injects a gateway-executed `web_search` tool so a chat model can
/// look things up through a registered `search` route.
/// Operators opt each model into a subset of these; nothing is granted by default.
pub const MODEL_BOONS: &[&str] = &[
    Boon::Vision.name(),
    Boon::StructuredOutput.name(),
    Boon::Compression.name(),
    Boon::Knowledge.name(),
    Boon::ImageGeneration.name(),
    Boon::Speculation.name(),
    Boon::WebSearch.name(),
];

/// One of the [`MODEL_BOONS`], for code that checks a grant: a typo in a
/// variant does not compile, where a typo in a string literal never matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Boon {
    Vision,
    StructuredOutput,
    Compression,
    Knowledge,
    ImageGeneration,
    Speculation,
    WebSearch,
}

impl Boon {
    /// The name the boon is granted and reported by.
    pub const fn name(self) -> &'static str {
        match self {
            Boon::Vision => "vision",
            Boon::StructuredOutput => "structured_output",
            Boon::Compression => "compression",
            Boon::Knowledge => "knowledge",
            Boon::ImageGeneration => "image_generation",
            Boon::Speculation => "speculation",
            Boon::WebSearch => "web_search",
        }
    }

    fn granted(self, boons: &[String]) -> bool {
        boons.iter().any(|b| b == self.name())
    }
}

/// True when `boon` is part of the fixed [`MODEL_BOONS`] vocabulary.
pub fn is_valid_boon(boon: &str) -> bool {
    MODEL_BOONS.contains(&boon)
}

/// Normalize an arbitrary list of boon strings to the canonical form used in
/// storage: trimmed, lowercased, restricted to the known vocabulary,
/// de-duplicated, and order-stable by first appearance.
pub fn normalize_boons<I, S>(boons: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for boon in boons {
        let b = boon.as_ref().trim().to_ascii_lowercase();
        if is_valid_boon(&b) && !out.contains(&b) {
            out.push(b);
        }
    }
    out
}

/// Fixed vocabulary of model modalities. Each value maps to a family of
/// OpenAI-compatible endpoints the model serves (see
/// `endpoint_matches_model_type` in the proxy). `chat` is the default.
///
/// `video` and `mesh` are job-based (`/v1/videos`, `/v1/meshes`): a create
/// returns a job id, and the result is fetched once the job completes. A
/// `mesh` model turns an image into a 3D model (binary glTF).
///
/// `search` is the odd one out: not a model but a web search tool, served on
/// `POST /v1/search` from an upstream that speaks the SearXNG JSON API.
pub const MODEL_TYPES: &[&str] = &[
    "chat",
    "embedding",
    "audio_transcription",
    "audio_speech",
    "image",
    "video",
    "mesh",
    "search",
];

/// The default modality assigned to a model when none is specified.
pub const DEFAULT_MODEL_TYPE: &str = "chat";

/// The modality of web search tools (see [`MODEL_TYPES`]).
pub const SEARCH_MODEL_TYPE: &str = "search";

/// The SearXNG search URL for a `search` route's `api_base`: the instance root,
/// with or without a trailing slash, gets `/search` appended; a base that
/// already ends in `/search` is used as is.
pub fn search_url(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    if base.ends_with("/search") {
        base.to_string()
    } else {
        format!("{base}/search")
    }
}

pub(super) fn default_model_type() -> String {
    DEFAULT_MODEL_TYPE.to_string()
}

/// Fixed vocabulary of capacity-tuning modes. `static` keeps the operator-set
/// `max_in_flight`; `tuned` lets auto-tune set it from a ramp probe;
/// `discovered` derives the pool size from the live backend (ready serving
/// replicas x per-replica concurrency), with `max_in_flight`
/// kept as the fallback when discovery has no answer.
pub const CAPACITY_MODES: &[&str] = &["static", "tuned", "discovered"];

/// The capacity mode whose pool size the gateway discovers from the backend.
pub const DISCOVERED_CAPACITY_MODE: &str = "discovered";

/// Fixed vocabulary of capacity sources for the `discovered` mode.
/// `endpoints` counts the model's own enabled, healthy endpoints and needs
/// nothing outside the gateway; `kubernetes` counts the ready endpoints of a
/// Service, read from its EndpointSlices through the Kubernetes API.
pub const CAPACITY_SOURCES: &[&str] = &["endpoints", "kubernetes"];

/// The capacity source assigned to a model when none is specified.
pub const DEFAULT_CAPACITY_SOURCE: &str = "endpoints";

pub(super) fn default_capacity_source() -> String {
    DEFAULT_CAPACITY_SOURCE.to_string()
}

pub(super) fn default_capacity_headroom() -> f64 {
    1.0
}

/// True when `source` is part of the fixed [`CAPACITY_SOURCES`] vocabulary.
pub fn is_valid_capacity_source(source: &str) -> bool {
    CAPACITY_SOURCES.contains(&source)
}

/// The default capacity mode assigned to a model when none is specified.
pub const DEFAULT_CAPACITY_MODE: &str = "static";

pub(super) fn default_capacity_mode() -> String {
    DEFAULT_CAPACITY_MODE.to_string()
}

/// True when `mode` is part of the fixed [`CAPACITY_MODES`] vocabulary.
pub fn is_valid_capacity_mode(mode: &str) -> bool {
    CAPACITY_MODES.contains(&mode)
}

/// Normalize a capacity-mode string to canonical storage form: trimmed and
/// lowercased, falling back to [`DEFAULT_CAPACITY_MODE`] when empty or unknown.
pub fn normalize_capacity_mode(mode: &str) -> String {
    let m = mode.trim().to_ascii_lowercase();
    if is_valid_capacity_mode(&m) {
        m
    } else {
        DEFAULT_CAPACITY_MODE.to_string()
    }
}

/// Fixed vocabulary of endpoint-selection modes. `failover` tries a model's
/// endpoints in priority order; `load_balance` spreads requests across them by
/// weight; `session_hash` pins a session to one endpoint (by a consistent hash
/// of its session/user key) for cache warmth, with the rest kept for failover.
/// Skipping unhealthy/disabled endpoints applies in all modes.
pub const ENDPOINT_SELECTION_MODES: &[&str] = &["failover", "load_balance", "session_hash"];

/// The default endpoint-selection mode assigned to a model when none is set.
pub const DEFAULT_ENDPOINT_SELECTION_MODE: &str = "failover";

pub(super) fn default_endpoint_selection_mode() -> String {
    DEFAULT_ENDPOINT_SELECTION_MODE.to_string()
}

/// Default base delay (ms) for retry exponential backoff.
pub const DEFAULT_RETRY_BACKOFF_MS: i64 = 200;

pub(super) fn default_retry_backoff_ms() -> i64 {
    DEFAULT_RETRY_BACKOFF_MS
}

/// Neutral value for a model's `auto`-router score multiplier.
pub const DEFAULT_ROUTE_BIAS: f64 = 1.0;

pub(super) fn default_route_bias() -> f64 {
    DEFAULT_ROUTE_BIAS
}

/// Models participate in `auto` routing unless an operator opts them out.
pub const DEFAULT_AUTO_ELIGIBLE: bool = true;

pub(super) fn default_auto_eligible() -> bool {
    DEFAULT_AUTO_ELIGIBLE
}

/// True when `mode` is part of the fixed [`ENDPOINT_SELECTION_MODES`] vocabulary.
pub fn is_valid_endpoint_selection_mode(mode: &str) -> bool {
    ENDPOINT_SELECTION_MODES.contains(&mode)
}

/// Normalize an endpoint-selection-mode string to canonical storage form:
/// trimmed and lowercased, falling back to [`DEFAULT_ENDPOINT_SELECTION_MODE`]
/// when empty or unknown.
pub fn normalize_endpoint_selection_mode(mode: &str) -> String {
    let m = mode.trim().to_ascii_lowercase();
    if is_valid_endpoint_selection_mode(&m) {
        m
    } else {
        DEFAULT_ENDPOINT_SELECTION_MODE.to_string()
    }
}

/// Both spellings of an already-built model-catalog URL, canonical first:
/// `…/models`, then `…/models/`.
///
/// The canonical spelling has no trailing slash, but some gateways mount
/// their list only at `/v1/models/` with trailing-slash redirects off, so the
/// canonical path 404s there with no redirect to follow. A caller that stops
/// at the first 404 silently loses every model behind such a gateway, and
/// model health reports an unreachable catalog. The rule lives here so a
/// third caller cannot miss it.
pub fn catalog_url_variants(models_url: &str) -> [String; 2] {
    let trimmed = models_url.trim_end_matches('/');
    [trimmed.to_string(), format!("{trimmed}/")]
}

/// [`catalog_url_variants`] for a caller holding an api_base rather than a
/// built URL: appends `models` to the base first.
pub fn catalog_urls(base: &str) -> [String; 2] {
    catalog_url_variants(&format!("{}/models", base.trim_end_matches('/')))
}

/// True when `model_type` is part of the fixed [`MODEL_TYPES`] vocabulary.
pub fn is_valid_model_type(model_type: &str) -> bool {
    MODEL_TYPES.contains(&model_type)
}

/// Normalize an arbitrary model-type string to the canonical form used in
/// storage: trimmed and lowercased, falling back to [`DEFAULT_MODEL_TYPE`]
/// when empty or outside the known vocabulary.
pub fn normalize_model_type(model_type: &str) -> String {
    let t = model_type.trim().to_ascii_lowercase();
    if is_valid_model_type(&t) {
        t
    } else {
        DEFAULT_MODEL_TYPE.to_string()
    }
}

/// Fixed vocabulary of weight/activation formats a model can be served in.
/// This is a *description* of the deployment, not part of the model's identity:
/// the client-facing `model_name` stays clean (`glm-5-3`) and the format it
/// happens to be quantized to is reported here and on the discovery endpoints,
/// so re-quantizing a deployment is an edit rather than a rename.
///
/// `unknown` is the default — a model whose format nobody has declared. `none`
/// is the distinct, deliberate statement that the weights are unquantized.
pub const QUANTIZATIONS: &[&str] = &[
    "unknown", "none", "fp16", "bf16", "fp8", "nvfp4", "mxfp4", "int8", "int4", "awq", "gptq",
    "gguf",
];

/// The format assigned to a model when none is declared.
pub const DEFAULT_QUANTIZATION: &str = "unknown";

pub(super) fn default_quantization() -> String {
    DEFAULT_QUANTIZATION.to_string()
}

/// True when `quantization` is part of the fixed [`QUANTIZATIONS`] vocabulary.
pub fn is_valid_quantization(quantization: &str) -> bool {
    QUANTIZATIONS.contains(&quantization)
}

/// Normalize an arbitrary quantization string to the canonical storage form:
/// trimmed and lowercased, with `-` and `_` separators folded away so the
/// spellings backends actually print (`FP8`, `fp8-e4m3`, `w4a16-awq`, `Q4_K_M`)
/// land on a vocabulary value. Anything still unrecognized becomes
/// [`DEFAULT_QUANTIZATION`] rather than being rejected — an undeclared format
/// is not a reason to fail a model edit.
pub fn normalize_quantization(quantization: &str) -> String {
    let raw = quantization.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return DEFAULT_QUANTIZATION.to_string();
    }
    if is_valid_quantization(&raw) {
        return raw;
    }
    // Collapse to alphanumerics so a suffixed or separator-spelled variant is
    // matched by the vocabulary value it prefixes. Longest first, so `nvfp4`
    // and `mxfp4` win over the `fp4` neither of them is.
    let flat: String = raw.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let mut vocab: Vec<&str> = QUANTIZATIONS
        .iter()
        .copied()
        .filter(|q| *q != "unknown" && *q != "none")
        .collect();
    vocab.sort_by_key(|q| std::cmp::Reverse(q.len()));
    for q in vocab {
        if flat.starts_with(q) || flat.ends_with(q) {
            return q.to_string();
        }
    }
    DEFAULT_QUANTIZATION.to_string()
}

/// Maximum extra client-facing names one model may answer to. Bounded because
/// every alias becomes its own Redis key and its own entry in the resolver's
/// warm set.
pub const MAX_MODEL_ALIASES: usize = 16;

/// Normalize a list of alias names to the canonical storage form: trimmed,
/// blanks dropped, de-duplicated, order-stable by first appearance, and capped
/// at [`MAX_MODEL_ALIASES`]. Matching is exact and case-sensitive, exactly as
/// it is for `model_name` — an alias is a name the resolver can be asked for,
/// so it obeys the same rules as the name it stands in for.
pub fn normalize_aliases<I, S>(aliases: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for alias in aliases {
        let a = alias.as_ref().trim().to_string();
        if a.is_empty() || out.len() >= MAX_MODEL_ALIASES {
            continue;
        }
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// Maximum variants one model may declare. Bounded for the same reason as
/// aliases: every variant is its own resolver key and discovery entry.
pub const MAX_MODEL_VARIANTS: usize = 8;

/// A separately named, opt-in way to call a model: the same route (same
/// backend, capacity, pool and prices) with extra boons turned on, e.g.
/// `glm-5-3-spec` = `glm-5-3` plus the `speculation` boon.
///
/// Exists because boons are otherwise per model: an operator who wants a boon
/// that changes who writes the answer (speculation) would have to turn it on
/// for everyone who calls the model. A variant lets callers opt in by name
/// while the plain name keeps serving the model's own output.
///
/// `boons` are added to the model's own — never removed — so a variant always
/// means "the model, plus these".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct ModelVariant {
    /// The name clients pass in `model`. Unique across every model's name,
    /// aliases and variants; exact, case-sensitive match.
    pub name: String,
    /// What a caller gets from this name, shown in the model listing.
    #[serde(default)]
    pub description: String,
    /// Boons from [`MODEL_BOONS`] turned on in addition to the model's own.
    #[serde(default)]
    pub boons: Vec<String>,
}

/// Normalize a variant list to the canonical storage form, mirroring
/// [`normalize_aliases`]: names and descriptions trimmed, blank names dropped,
/// duplicate names dropped (first wins), boons normalized with
/// [`normalize_boons`], and the list capped at [`MAX_MODEL_VARIANTS`]. The
/// Management API rejects these cases with an error before they get here;
/// this is the storage-side backstop for every other write path.
pub fn normalize_variants<'a, I>(variants: I) -> Vec<ModelVariant>
where
    I: IntoIterator<Item = &'a ModelVariant>,
{
    let mut out: Vec<ModelVariant> = Vec::new();
    for v in variants {
        let name = v.name.trim();
        if name.is_empty() || out.len() >= MAX_MODEL_VARIANTS {
            continue;
        }
        if out.iter().any(|o| o.name == name) {
            continue;
        }
        out.push(ModelVariant {
            name: name.to_string(),
            description: v.description.trim().to_string(),
            boons: normalize_boons(&v.boons),
        });
    }
    out
}

/// Where a model is in its life. See [`ModelLifecycle`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ModelStatus {
    /// Served by name, but not advertised: left out of `/v1/models`,
    /// `/model/info` and the search tool list, and never picked by `auto`.
    /// For trying a new model before callers can find it.
    Staged,
    /// Served normally.
    #[default]
    Active,
    /// Still served, but every response says so (`Deprecation`, and `Sunset`
    /// once a retirement date is set) and names the replacement. Listed in
    /// `/v1/models` as deprecated; never picked by `auto`.
    Deprecated,
    /// No longer served: requests are refused with `410 Gone` naming the
    /// replacement, or answered by the replacement when `redirect` is on.
    /// Hidden from `/v1/models`.
    Retired,
}

impl ModelStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelStatus::Staged => "staged",
            ModelStatus::Active => "active",
            ModelStatus::Deprecated => "deprecated",
            ModelStatus::Retired => "retired",
        }
    }

    /// Whether the discovery endpoints advertise a model in this status: a
    /// staged model is not advertised yet, and a retired one no longer is.
    pub fn listed(self) -> bool {
        matches!(self, ModelStatus::Active | ModelStatus::Deprecated)
    }
}

/// Longest operator note carried with a lifecycle status. It is repeated in
/// every refusal of a retired model, so it is kept to a sentence or two.
pub const MAX_LIFECYCLE_NOTE_CHARS: usize = 500;

/// A model's lifecycle: whether it is still served, what replaces it, and
/// when it goes away. Stored as one JSON object on the model (like
/// `variants`), so a field added later needs no migration, and the default
/// (`{}`) is an active model with nothing to say.
///
/// A deprecated model whose `retire_at` has passed is retired: the operator
/// sets the date once and the gateway retires the model on time
/// ([`ModelLifecycle::status_at`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct ModelLifecycle {
    #[serde(default)]
    pub status: ModelStatus,
    /// `model_name` of the model callers should move to. Empty when none is
    /// named.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub replacement: String,
    /// When the model stops being served. Sent as the `Sunset` header while
    /// deprecated; once it passes, the model is retired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the status last changed. Sent as the `Deprecation` date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The operator's word to callers, repeated with the status (why, or where
    /// to ask).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Once retired, answer with `replacement` instead of refusing. The
    /// response still says it was redirected; off by default because a
    /// different model can answer differently, and a caller pinned to this
    /// one should find out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redirect: bool,
}

impl ModelLifecycle {
    /// Check what the lifecycle says on its own: a note within
    /// [`MAX_LIFECYCLE_NOTE_CHARS`], and a redirect only with a replacement.
    /// Whether the replacement exists and can stand in is the caller's to
    /// check, against the registry.
    pub fn validate(&self) -> Result<(), String> {
        if self.note.chars().count() > MAX_LIFECYCLE_NOTE_CHARS {
            return Err(format!(
                "note is longer than {MAX_LIFECYCLE_NOTE_CHARS} characters"
            ));
        }
        if self.redirect && self.replacement.is_empty() {
            return Err("redirect needs a replacement to send requests to".to_string());
        }
        Ok(())
    }

    /// The status in force at `now`: the stored one, except that a
    /// deprecated model whose retirement date has passed is retired.
    pub fn status_at(&self, now: chrono::DateTime<chrono::Utc>) -> ModelStatus {
        match self.status {
            ModelStatus::Deprecated if self.retire_at.is_some_and(|at| at <= now) => {
                ModelStatus::Retired
            }
            status => status,
        }
    }

    /// When the model was (or will be) retired, for messages: the retirement
    /// date if one was set, otherwise the moment it was marked retired.
    pub fn retired_on(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.retire_at.or(match self.status {
            ModelStatus::Retired => self.changed_at,
            _ => None,
        })
    }
}

/// `base` followed by every boon of `extra` not already in it: the boons a
/// variant request runs with. Order-stable and de-duplicated, so the model's
/// own boons keep their order and a variant that repeats one adds nothing.
pub fn union_boons(base: &[String], extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(base.len() + extra.len());
    for b in base.iter().chain(extra) {
        if !out.contains(b) {
            out.push(b.clone());
        }
    }
    out
}

/// Operator-configured headers added to every request sent to a model's
/// upstream. Keyed by lowercase header name; a `BTreeMap` so the stored,
/// cached, and compared forms are order-stable.
pub type UpstreamHeaders = std::collections::BTreeMap<String, String>;

/// A write to a model's upstream headers: the full set of names the model
/// should carry. A string sets that header's value; `null` keeps the value
/// already stored under that name, so a client that was only ever shown the
/// names (values are write-only) can add or drop one header without
/// re-sending the others. A stored name missing from the write is removed.
pub type UpstreamHeadersWrite = std::collections::BTreeMap<String, Option<String>>;

/// Maximum upstream headers per model.
pub const MAX_UPSTREAM_HEADERS: usize = 32;
/// Maximum length of one upstream header value, in bytes.
pub const MAX_UPSTREAM_HEADER_VALUE_LEN: usize = 4096;

/// Header names an operator may not set on a model. Hop-by-hop headers belong
/// to one connection, not to the request. `authorization` is owned by the
/// model's `api_key`. `host`, `content-length`, and `content-type` describe
/// the body the gateway itself rebuilds (a re-serialized JSON body, or a
/// multipart form with a fresh boundary), and `accept-encoding` is stripped so
/// the gateway can read the response it meters.
pub const DENIED_UPSTREAM_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "authorization",
    "host",
    "content-length",
    "content-type",
    "accept-encoding",
];

/// RFC 9110 token characters, the only ones a header name may contain.
fn is_header_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Validate one upstream header and return its canonical (lowercase) name.
/// Values must be visible ASCII, space, or tab: no line breaks, so a value
/// can never smuggle a second header.
pub fn validate_upstream_header(name: &str, value: &str) -> Result<String, String> {
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() || !name.bytes().all(is_header_token_byte) {
        return Err(format!(
            "upstream header name '{name}' is not a valid header name"
        ));
    }
    if DENIED_UPSTREAM_HEADERS.contains(&name.as_str()) {
        return Err(format!(
            "upstream header '{name}' cannot be set per model (the gateway owns it; \
             use api_key for upstream auth)"
        ));
    }
    if value.len() > MAX_UPSTREAM_HEADER_VALUE_LEN {
        return Err(format!(
            "upstream header '{name}' is longer than {MAX_UPSTREAM_HEADER_VALUE_LEN} bytes"
        ));
    }
    if !value
        .bytes()
        .all(|b| b == b'\t' || (b' '..=b'~').contains(&b))
    {
        return Err(format!(
            "upstream header '{name}' has a value with characters a header cannot carry"
        ));
    }
    Ok(name)
}

/// Apply an [`UpstreamHeadersWrite`] to the headers stored today, validating
/// every entry. Names are case-insensitive: two spellings of one name in a
/// single write are rejected rather than one silently winning.
pub fn merge_upstream_headers(
    stored: &UpstreamHeaders,
    write: &UpstreamHeadersWrite,
) -> Result<UpstreamHeaders, String> {
    if write.len() > MAX_UPSTREAM_HEADERS {
        return Err(format!(
            "at most {MAX_UPSTREAM_HEADERS} upstream headers may be set per model"
        ));
    }
    let mut out = UpstreamHeaders::new();
    for (raw_name, value) in write {
        let value = match value {
            Some(v) => v.trim().to_string(),
            None => {
                let key = raw_name.trim().to_ascii_lowercase();
                stored.get(&key).cloned().ok_or_else(|| {
                    format!("upstream header '{key}' has no stored value to keep; send a value")
                })?
            }
        };
        let name = validate_upstream_header(raw_name, &value)?;
        if out.insert(name.clone(), value).is_some() {
            return Err(format!("upstream header '{name}' is given more than once"));
        }
    }
    Ok(out)
}

/// Header names only, for API responses: values are write-only because an
/// operator may put a credential in one.
pub fn upstream_header_names(headers: &UpstreamHeaders) -> Vec<String> {
    headers.keys().cloned().collect()
}

/// Normalize an arbitrary list of tag strings to the canonical form used for
/// matching: trimmed, lowercased, restricted to the known vocabulary,
/// de-duplicated, and order-stable by first appearance. Any `:1|2|3` strength
/// suffix is stripped so the existing overlap match in the router keeps
/// working untouched; use [`normalize_tag_levels`] to recover the ladder.
pub fn normalize_tags<I, S>(tags: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        if let Some((base, _)) = parse_tag_level(tag.as_ref()) {
            if !out.contains(&base) {
                out.push(base);
            }
        }
    }
    out
}

/// The *explicitly* declared strength ladder: one entry per valid tag that
/// carries a `:1|2|3` suffix, first occurrence wins. A bare tag declares
/// nothing here — under `TierSource::Hybrid` it falls through to the
/// cost-derived level, which is what makes tiering useful on a fleet whose
/// operator never typed a suffix. (`TierSource::Declared` still reads a
/// missing declaration as level 1, so the old "bare = 1" reading survives
/// where it was the documented contract.)
pub fn declared_tag_levels<I, S>(tags: I) -> Vec<(String, u8)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<(String, u8)> = Vec::new();
    for tag in tags {
        let raw = tag.as_ref();
        if !raw.contains(':') {
            continue;
        }
        if let Some((base, level)) = parse_tag_level(raw) {
            if !out.iter().any(|(b, _)| b == &base) {
                out.push((base, level));
            }
        }
    }
    out
}

/// Canonical storage form of a tag list: trimmed, lowercased, validated
/// against the vocabulary, de-duplicated by base (first occurrence wins) —
/// and, unlike the old collapse-to-bare form, an explicit `:1` suffix is
/// PRESERVED. Bare and `tag:1` mean different things under
/// `TierSource::Hybrid` (auto-derived vs pinned weak), so the save path must
/// not erase the distinction the operator typed.
pub fn canonical_tags<I, S>(tags: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for tag in tags {
        let raw = tag.as_ref().trim().to_ascii_lowercase();
        if let Some((base, level)) = parse_tag_level(&raw) {
            if seen.contains(&base) {
                continue;
            }
            seen.push(base.clone());
            if raw.contains(':') {
                out.push(format!("{base}:{level}"));
            } else {
                out.push(base);
            }
        }
    }
    out
}

/// The declared strength ladder: one entry per valid tag, first occurrence
/// wins. Companion to [`normalize_tags`], which returns the same tags stripped
/// down to their bare vocabulary form for overlap matching.
pub fn normalize_tag_levels<I, S>(tags: I) -> Vec<(String, u8)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<(String, u8)> = Vec::new();
    for tag in tags {
        if let Some((base, level)) = parse_tag_level(tag.as_ref()) {
            if !out.iter().any(|(b, _)| b == &base) {
                out.push((base, level));
            }
        }
    }
    out
}

/// Normalize a list of MCP server names granted to a model: trimmed,
/// de-duplicated, empties dropped, order-stable by first appearance. Unlike
/// boons/tags there is no fixed vocabulary — server names are operator-defined.
pub fn normalize_tool_servers<I, S>(servers: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for server in servers {
        let s = server.as_ref().trim().to_string();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Maximum request fields one model may refuse.
pub const MAX_REJECTED_REQUEST_FIELDS: usize = 32;

/// Longest request field name accepted in a model's rejected list.
pub const MAX_REJECTED_REQUEST_FIELD_LEN: usize = 128;

/// Fields every request needs, so refusing one would refuse every request.
const UNREJECTABLE_REQUEST_FIELDS: &[&str] = &["model"];

/// Check and normalize a model's rejected request fields: names trimmed,
/// empty entries dropped, duplicates removed, order kept. Names stay
/// case-sensitive, as JSON keys are. Too many names, a name that is too long,
/// or one every request needs (`model`) is an error.
pub fn validate_rejected_request_fields<I, S>(fields: I) -> Result<Vec<String>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for field in fields {
        let f = field.as_ref().trim();
        if f.is_empty() || out.iter().any(|o| o == f) {
            continue;
        }
        if f.len() > MAX_REJECTED_REQUEST_FIELD_LEN {
            return Err(format!(
                "rejected request field '{f}' is longer than {MAX_REJECTED_REQUEST_FIELD_LEN} characters"
            ));
        }
        if UNREJECTABLE_REQUEST_FIELDS.contains(&f) {
            return Err(format!(
                "'{f}' cannot be a rejected request field: every request needs it"
            ));
        }
        out.push(f.to_string());
    }
    if out.len() > MAX_REJECTED_REQUEST_FIELDS {
        return Err(format!(
            "at most {MAX_REJECTED_REQUEST_FIELDS} rejected request fields per model"
        ));
    }
    Ok(out)
}

/// The lenient form of [`validate_rejected_request_fields`] for stored
/// values: anything it would refuse is dropped instead.
pub fn normalize_rejected_request_fields<I, S>(fields: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for field in fields {
        let f = field.as_ref().trim();
        if f.is_empty()
            || f.len() > MAX_REJECTED_REQUEST_FIELD_LEN
            || UNREJECTABLE_REQUEST_FIELDS.contains(&f)
            || out.iter().any(|o| o == f)
            || out.len() >= MAX_REJECTED_REQUEST_FIELDS
        {
            continue;
        }
        out.push(f.to_string());
    }
    out
}

/// The first of `fields` a request body carries, at the top level or inside
/// an `extra_body` object (which some clients send as is), or `None`.
pub fn rejected_request_field<'a>(
    body: &serde_json::Value,
    fields: &'a [String],
) -> Option<&'a str> {
    if fields.is_empty() {
        return None;
    }
    let obj = body.as_object()?;
    let extra = obj.get("extra_body").and_then(serde_json::Value::as_object);
    fields
        .iter()
        .find(|f| obj.contains_key(f.as_str()) || extra.is_some_and(|e| e.contains_key(f.as_str())))
        .map(String::as_str)
}

/// Longest hold a model may set for session affinity, in milliseconds.
pub const MAX_SESSION_AFFINITY_HOLD_MS: u64 = 60_000;

/// Longest bypass limit a model may set for session affinity, in seconds.
pub const MAX_SESSION_AFFINITY_BYPASS_SECS: u64 = 3_600;

/// A model's own session affinity setting. Stored as one JSON object on the
/// model, read tolerantly, so `{}` (the default) follows the gateway: its
/// `OBLETH_FAIRSHARE_SESSION_AFFINITY` switch and values. Each field set here
/// replaces the gateway's for this model's pool alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ModelSessionAffinity {
    /// On or off for this model; absent follows the gateway's switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// How long a slot is held for a session's next step, in ms (0 holds
    /// nothing, at most 60000); absent uses the gateway's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<u64>,
    /// The most of the pool's slots held at once, in percent (0 to 100);
    /// absent uses the gateway's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_held_percent: Option<u8>,
    /// Seconds after which nothing passes a waiting request and held slots
    /// give way to it (1 to 3600); absent uses the gateway's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bypass_secs: Option<u64>,
}

impl ModelSessionAffinity {
    /// Whether the model follows the gateway in everything.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Refuse values out of range.
    pub fn validate(&self) -> Result<(), String> {
        if self
            .hold_ms
            .is_some_and(|ms| ms > MAX_SESSION_AFFINITY_HOLD_MS)
        {
            return Err(format!(
                "session_affinity.hold_ms must be at most {MAX_SESSION_AFFINITY_HOLD_MS}"
            ));
        }
        if self.max_held_percent.is_some_and(|p| p > 100) {
            return Err("session_affinity.max_held_percent must be 0 to 100".into());
        }
        if self
            .max_bypass_secs
            .is_some_and(|s| s == 0 || s > MAX_SESSION_AFFINITY_BYPASS_SECS)
        {
            return Err(format!(
                "session_affinity.max_bypass_secs must be 1 to {MAX_SESSION_AFFINITY_BYPASS_SECS}"
            ));
        }
        Ok(())
    }

    /// What the model's pool runs with, given whether the gateway's switch
    /// is on and the gateway's values: `None` when affinity is off for it.
    /// The hold is kept within the bypass limit, as the gateway's is.
    pub fn resolve(
        &self,
        gateway_on: bool,
        gateway: &crate::config::SessionAffinityConfig,
    ) -> Option<crate::config::SessionAffinityConfig> {
        if !self.enabled.unwrap_or(gateway_on) {
            return None;
        }
        let mut c = *gateway;
        if let Some(secs) = self.max_bypass_secs.filter(|s| *s > 0) {
            c.max_bypass =
                std::time::Duration::from_secs(secs.min(MAX_SESSION_AFFINITY_BYPASS_SECS));
        }
        if let Some(ms) = self.hold_ms {
            c.hold = std::time::Duration::from_millis(ms.min(MAX_SESSION_AFFINITY_HOLD_MS));
        }
        c.hold = c.hold.min(c.max_bypass);
        if let Some(p) = self.max_held_percent {
            c.max_held_percent = p.min(100);
        }
        Some(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_urls_try_the_canonical_path_then_the_trailing_slash() {
        assert_eq!(
            catalog_urls("http://gateway/v1"),
            [
                "http://gateway/v1/models".to_string(),
                "http://gateway/v1/models/".to_string(),
            ]
        );
    }

    #[test]
    fn catalog_urls_normalize_trailing_slashes_on_the_base() {
        assert_eq!(
            catalog_urls("http://gateway/v1/"),
            catalog_urls("http://gateway/v1")
        );
    }

    #[test]
    fn catalog_url_variants_offer_both_spellings_of_a_built_url() {
        assert_eq!(
            catalog_url_variants("http://gateway/v1/models"),
            [
                "http://gateway/v1/models".to_string(),
                "http://gateway/v1/models/".to_string(),
            ]
        );
        // Idempotent: a caller that already added the slash gets the same pair.
        assert_eq!(
            catalog_url_variants("http://gateway/v1/models/"),
            catalog_url_variants("http://gateway/v1/models")
        );
    }

    #[test]
    fn declared_levels_come_only_from_explicit_suffixes() {
        let tags = vec![
            "coding".to_string(),
            "math:2".to_string(),
            "vision:1".to_string(),
        ];
        let declared = declared_tag_levels(&tags);
        // Bare "coding" declares nothing: under hybrid tiering it derives from
        // cost rank instead of silently pinning the model to level 1.
        assert_eq!(
            declared,
            vec![("math".to_string(), 2), ("vision".to_string(), 1)]
        );
    }

    #[test]
    fn canonical_tags_preserve_an_explicit_level_one() {
        // bare = "derive my level", tag:1 = "pinned weak on purpose". The save
        // path must keep the operator's distinction.
        let tags = vec![
            "coding:1".to_string(),
            "math".to_string(),
            "vision:3".to_string(),
            "Vision:2".to_string(),  // duplicate base: first occurrence wins
            "astrology".to_string(), // not in the vocabulary: dropped
        ];
        assert_eq!(
            canonical_tags(&tags),
            vec![
                "coding:1".to_string(),
                "math".to_string(),
                "vision:3".to_string()
            ]
        );
    }

    #[test]
    fn bare_tag_is_level_one() {
        assert_eq!(parse_tag_level("coding"), Some(("coding".to_string(), 1)));
    }

    #[test]
    fn suffixed_tag_parses_its_level() {
        assert_eq!(parse_tag_level("coding:3"), Some(("coding".to_string(), 3)));
    }

    #[test]
    fn unknown_base_is_rejected_with_or_without_a_suffix() {
        assert_eq!(parse_tag_level("astrology"), None);
        assert_eq!(parse_tag_level("astrology:3"), None);
    }

    #[test]
    fn out_of_range_level_clamps_into_the_ladder() {
        assert_eq!(parse_tag_level("coding:0"), Some(("coding".to_string(), 1)));
        assert_eq!(parse_tag_level("coding:9"), Some(("coding".to_string(), 3)));
        assert_eq!(parse_tag_level("coding:x"), Some(("coding".to_string(), 1)));
    }

    #[test]
    fn normalize_keeps_bare_tags_so_overlap_match_is_unchanged() {
        let got = normalize_tags(["Coding:3", "  MATH ", "astrology"]);
        assert_eq!(
            got,
            vec!["coding".to_string(), "math".to_string()],
            "normalize_tags must still yield the bare vocabulary for overlap matching"
        );
    }

    #[test]
    fn normalize_levels_extracts_the_ladder() {
        let got = normalize_tag_levels(["coding:3", "math", "astrology:2"]);
        assert_eq!(
            got,
            vec![("coding".to_string(), 3), ("math".to_string(), 1)]
        );
    }

    #[test]
    fn duplicate_declared_levels_keep_the_first_occurrence() {
        // normalize_tag_levels dedups by first occurrence: ["coding:3","coding:1"] -> 3.
        let got = normalize_tag_levels(["coding:3", "coding:1"]);
        assert_eq!(got, vec![("coding".to_string(), 3)]);
    }

    #[test]
    fn normalize_boons_accepts_new_vocabulary() {
        // The retired `tools` boon is dropped along with any other unknown value;
        // `compression` and `speculation` are part of the vocabulary and kept.
        let boons = normalize_boons([
            " structured_output ",
            "vision",
            "compression",
            "bogus",
            "tools",
            "Speculation",
        ]);
        assert_eq!(
            boons,
            vec!["structured_output", "vision", "compression", "speculation"]
        );
    }

    #[test]
    fn knowledge_is_a_recognized_boon() {
        assert!(is_valid_boon("knowledge"));
        let boons = normalize_boons(["knowledge", "knowledge", "bogus"]);
        assert_eq!(boons, vec!["knowledge"]);
    }

    #[test]
    fn resolved_model_without_collections_still_deserializes() {
        // Redis payloads cached by an older build lack the new field.
        let json = r#"{"model_name":"m","upstream_model":"u","api_base":"http://x",
                       "api_key":null,"model_type":"chat","admission_weight":100,
                       "max_in_flight":null,"enabled":true,"cache_enabled":false,
                       "cache_ttl_secs":0}"#;
        let m: ResolvedModel = serde_json::from_str(json).expect("deserialize");
        assert!(m.knowledge_collections.is_empty());
    }

    #[test]
    fn image_generation_boon_is_in_the_vocabulary() {
        assert!(is_valid_boon("image_generation"));
        assert_eq!(
            normalize_boons(["Image_Generation", "image_generation", "bogus"]),
            vec!["image_generation".to_string()]
        );
    }

    #[test]
    fn web_search_boon_is_in_the_vocabulary() {
        assert!(is_valid_boon("web_search"));
        // Each typed boon is exactly one name of the vocabulary.
        let all = [
            Boon::Vision,
            Boon::StructuredOutput,
            Boon::Compression,
            Boon::Knowledge,
            Boon::ImageGeneration,
            Boon::Speculation,
            Boon::WebSearch,
        ];
        assert_eq!(all.map(Boon::name).as_slice(), MODEL_BOONS);
        for name in MODEL_BOONS {
            let granted: Vec<&str> = all
                .into_iter()
                .filter(|b| b.granted(&[name.to_string()]))
                .map(Boon::name)
                .collect();
            assert_eq!(granted, vec![*name]);
        }
        assert_eq!(
            normalize_boons(["Web_Search", "web_search", "vision"]),
            vec!["web_search".to_string(), "vision".to_string()]
        );
    }

    #[test]
    fn search_is_a_model_type_and_survives_storage() {
        assert!(is_valid_model_type(SEARCH_MODEL_TYPE));
        assert_eq!(normalize_model_type(" Search "), SEARCH_MODEL_TYPE);
    }

    #[test]
    fn search_url_appends_search_to_the_instance_root_once() {
        assert_eq!(
            search_url("http://searxng:8080"),
            "http://searxng:8080/search"
        );
        assert_eq!(
            search_url("http://searxng:8080/"),
            "http://searxng:8080/search"
        );
        assert_eq!(
            search_url("https://example.org/searxng/search/"),
            "https://example.org/searxng/search"
        );
    }

    #[test]
    fn quantization_vocabulary_values_are_all_valid() {
        for q in QUANTIZATIONS {
            assert!(is_valid_quantization(q), "{q}");
            // Every vocabulary value is its own normal form, or a round trip
            // through storage would rewrite it.
            assert_eq!(normalize_quantization(q), *q, "{q}");
        }
        assert!(is_valid_quantization(DEFAULT_QUANTIZATION));
    }

    #[test]
    fn normalize_quantization_folds_the_spellings_backends_print() {
        // Case and separators are noise.
        assert_eq!(normalize_quantization("FP8"), "fp8");
        assert_eq!(normalize_quantization("  BF16 "), "bf16");
        // Suffixed and prefixed variants land on the format they describe.
        assert_eq!(normalize_quantization("fp8-e4m3"), "fp8");
        assert_eq!(normalize_quantization("w4a16-awq"), "awq");
        assert_eq!(normalize_quantization("Q4_K_M-gguf"), "gguf");
        // The two 4-bit float formats stay distinct: neither collapses into
        // the other, and neither is read as a bare `fp4`.
        assert_eq!(normalize_quantization("MXFP4"), "mxfp4");
        assert_eq!(normalize_quantization("nvfp4-a16"), "nvfp4");
        // Undeclared is not the same claim as unquantized.
        assert_eq!(normalize_quantization(""), "unknown");
        assert_eq!(normalize_quantization("   "), "unknown");
        assert_eq!(normalize_quantization("something-else"), "unknown");
        assert_eq!(normalize_quantization("none"), "none");
    }

    #[test]
    fn upstream_headers_are_validated_lowercased_and_merged() {
        let stored: UpstreamHeaders = [("x-token".to_string(), "old".to_string())].into();

        // Names fold to lowercase; `null` keeps the stored value; the rest is replaced.
        let write: UpstreamHeadersWrite = [
            ("X-Token".to_string(), None),
            ("X-Routing-Hint".to_string(), Some(" sticky ".into())),
        ]
        .into();
        let merged = merge_upstream_headers(&stored, &write).unwrap();
        assert_eq!(merged["x-token"], "old");
        assert_eq!(merged["x-routing-hint"], "sticky");
        assert_eq!(
            upstream_header_names(&merged),
            vec!["x-routing-hint".to_string(), "x-token".to_string()]
        );
        assert!(
            merge_upstream_headers(&stored, &UpstreamHeadersWrite::new())
                .unwrap()
                .is_empty()
        );

        // The gateway owns auth, framing, and connection headers.
        for denied in DENIED_UPSTREAM_HEADERS {
            let w: UpstreamHeadersWrite = [(denied.to_uppercase(), Some("v".into()))].into();
            assert!(merge_upstream_headers(&stored, &w).is_err(), "{denied}");
        }
        let bad = |name: &str, value: Option<&str>| {
            let w: UpstreamHeadersWrite = [(name.to_string(), value.map(str::to_string))].into();
            merge_upstream_headers(&stored, &w).is_err()
        };
        assert!(bad("x-new", None), "nothing stored to keep");
        assert!(bad("bad name", Some("v")));
        assert!(bad("", Some("v")));
        assert!(bad("x-split", Some("a\r\nx-injected: 1")));
        assert!(bad(
            "x-long",
            Some(&"v".repeat(MAX_UPSTREAM_HEADER_VALUE_LEN + 1))
        ));
        let dup: UpstreamHeadersWrite = [
            ("x-a".to_string(), Some("1".into())),
            ("X-A".to_string(), Some("2".into())),
        ]
        .into();
        assert!(merge_upstream_headers(&stored, &dup).is_err());
        let many: UpstreamHeadersWrite = (0..=MAX_UPSTREAM_HEADERS)
            .map(|i| (format!("x-{i}"), Some("v".to_string())))
            .collect();
        assert!(merge_upstream_headers(&stored, &many).is_err());
    }

    #[test]
    fn normalize_aliases_trims_dedupes_and_caps() {
        assert_eq!(
            normalize_aliases(["glm-5-3-fp8", "  glm-5-3-mxfp4  ", "", "glm-5-3-fp8"]),
            vec!["glm-5-3-fp8".to_string(), "glm-5-3-mxfp4".to_string()]
        );
        // Case is significant, because alias lookup is exact — the same rule
        // `model_name` follows.
        assert_eq!(
            normalize_aliases(["Glm-5-3", "glm-5-3"]),
            vec!["Glm-5-3".to_string(), "glm-5-3".to_string()]
        );
        // Bounded: every alias becomes its own resolver key.
        let many: Vec<String> = (0..MAX_MODEL_ALIASES + 5)
            .map(|i| format!("a{i}"))
            .collect();
        assert_eq!(normalize_aliases(&many).len(), MAX_MODEL_ALIASES);
    }

    #[test]
    fn model_session_affinity_follows_the_gateway_unless_set() {
        use crate::config::SessionAffinityConfig;
        let gateway = SessionAffinityConfig::settings_from_values(None, None, None, None, None);
        let follow = ModelSessionAffinity::default();
        assert!(follow.is_default());
        assert_eq!(follow.resolve(false, &gateway), None);
        assert_eq!(follow.resolve(true, &gateway), Some(gateway));

        let off = ModelSessionAffinity {
            enabled: Some(false),
            ..Default::default()
        };
        assert_eq!(off.resolve(true, &gateway), None);

        let own = ModelSessionAffinity {
            enabled: Some(true),
            hold_ms: Some(5_000),
            max_held_percent: Some(50),
            max_bypass_secs: Some(120),
        };
        let c = own.resolve(false, &gateway).expect("on for this model");
        assert_eq!(c.hold, std::time::Duration::from_millis(5_000));
        assert_eq!(c.max_held_percent, 50);
        assert_eq!(c.max_bypass, std::time::Duration::from_secs(120));
        assert_eq!(c.warm_ttl, gateway.warm_ttl);
        assert_eq!(c.max_jumps, gateway.max_jumps);

        // The hold stays within the bypass limit.
        let short = ModelSessionAffinity {
            enabled: Some(true),
            hold_ms: Some(10_000),
            max_bypass_secs: Some(2),
            ..Default::default()
        };
        assert_eq!(
            short.resolve(false, &gateway).unwrap().hold,
            std::time::Duration::from_secs(2)
        );

        assert!(own.validate().is_ok());
        for bad in [
            ModelSessionAffinity {
                hold_ms: Some(60_001),
                ..Default::default()
            },
            ModelSessionAffinity {
                max_held_percent: Some(101),
                ..Default::default()
            },
            ModelSessionAffinity {
                max_bypass_secs: Some(0),
                ..Default::default()
            },
            ModelSessionAffinity {
                max_bypass_secs: Some(3_601),
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        // Stored as only what is set; unknown and missing fields are read
        // tolerantly.
        assert_eq!(serde_json::to_string(&follow).unwrap(), "{}");
        let read: ModelSessionAffinity =
            serde_json::from_str(r#"{"enabled":true,"later_field":1}"#).unwrap();
        assert_eq!(read.enabled, Some(true));
    }

    #[test]
    fn rejected_request_fields_are_trimmed_deduped_and_checked() {
        assert_eq!(
            validate_rejected_request_fields([" bad_words ", "", "bad_words", "Bad_Words"])
                .unwrap(),
            vec!["bad_words".to_string(), "Bad_Words".to_string()]
        );
        assert!(validate_rejected_request_fields(["model"]).is_err());
        assert!(
            validate_rejected_request_fields(["x".repeat(MAX_REJECTED_REQUEST_FIELD_LEN + 1)])
                .is_err()
        );
        let many: Vec<String> = (0..MAX_REJECTED_REQUEST_FIELDS + 1)
            .map(|i| format!("f{i}"))
            .collect();
        assert!(validate_rejected_request_fields(&many).is_err());
        // The stored form drops what validation refuses instead of failing.
        assert_eq!(
            normalize_rejected_request_fields(["model", " bad_words", "bad_words"]),
            vec!["bad_words".to_string()]
        );
        assert_eq!(
            normalize_rejected_request_fields(&many).len(),
            MAX_REJECTED_REQUEST_FIELDS
        );
    }

    #[test]
    fn rejected_request_field_finds_top_level_and_extra_body_keys() {
        let fields = vec!["bad_words".to_string(), "logit_bias".to_string()];
        let plain = serde_json::json!({"model": "m", "messages": [], "temperature": 0});
        assert_eq!(rejected_request_field(&plain, &fields), None);
        let top = serde_json::json!({"model": "m", "logit_bias": {}, "bad_words": ["x"]});
        assert_eq!(rejected_request_field(&top, &fields), Some("bad_words"));
        let extra = serde_json::json!({"model": "m", "extra_body": {"logit_bias": {}}});
        assert_eq!(rejected_request_field(&extra, &fields), Some("logit_bias"));
        // Keys are case-sensitive, and a field named only inside a message
        // or a nested object other than `extra_body` is not a request field.
        let nested = serde_json::json!({
            "model": "m",
            "Bad_Words": ["x"],
            "messages": [{"role": "user", "content": "hi", "bad_words": ["x"]}],
            "metadata": {"bad_words": "x"}
        });
        assert_eq!(rejected_request_field(&nested, &fields), None);
        // Nothing configured refuses nothing, whatever the body.
        assert_eq!(rejected_request_field(&top, &[]), None);
        assert_eq!(
            rejected_request_field(&serde_json::Value::Null, &fields),
            None
        );
    }

    #[test]
    fn addressable_names_lists_the_canonical_name_first() {
        let mut model = resolved_fixture();
        assert_eq!(
            model.addressable_names().collect::<Vec<_>>(),
            vec!["glm-5-3", "glm-5-3-fp8", "glm-5-3-mxfp4"]
        );
        // A model with no aliases is still addressable by its own name, so a
        // publish loop over this iterator is never a no-op.
        model.aliases.clear();
        assert_eq!(
            model.addressable_names().collect::<Vec<_>>(),
            vec!["glm-5-3"]
        );
        // Variants are resolver keys too, after the aliases.
        model.variants = vec![variant("glm-5-3-spec", &["speculation"])];
        assert_eq!(
            model.addressable_names().collect::<Vec<_>>(),
            vec!["glm-5-3", "glm-5-3-spec"]
        );
    }

    fn variant(name: &str, boons: &[&str]) -> ModelVariant {
        ModelVariant {
            name: name.into(),
            description: String::new(),
            boons: boons.iter().map(|b| b.to_string()).collect(),
        }
    }

    #[test]
    fn lifecycle_defaults_to_active_and_reads_an_empty_object() {
        let parsed: ModelLifecycle = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(parsed, ModelLifecycle::default());
        assert_eq!(parsed.status, ModelStatus::Active);
        // An active lifecycle stores as just its status.
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            serde_json::json!({ "status": "active" })
        );
    }

    #[test]
    fn a_staged_model_stays_staged_and_is_not_listed() {
        let parsed: ModelLifecycle =
            serde_json::from_value(serde_json::json!({ "status": "staged" })).unwrap();
        assert_eq!(parsed.status, ModelStatus::Staged);
        // A stray date does not move a staged model anywhere.
        let dated = ModelLifecycle {
            retire_at: Some(chrono::Utc::now() - chrono::Duration::days(1)),
            ..parsed
        };
        assert_eq!(dated.status_at(chrono::Utc::now()), ModelStatus::Staged);
        assert!(!ModelStatus::Staged.listed());
        assert!(ModelStatus::Active.listed());
        assert!(ModelStatus::Deprecated.listed());
        assert!(!ModelStatus::Retired.listed());
    }

    #[test]
    fn a_deprecated_model_retires_itself_when_its_date_passes() {
        let now = chrono::Utc::now();
        let mut l = ModelLifecycle {
            status: ModelStatus::Deprecated,
            retire_at: Some(now + chrono::Duration::days(14)),
            ..Default::default()
        };
        assert_eq!(l.status_at(now), ModelStatus::Deprecated);
        assert_eq!(
            l.status_at(now + chrono::Duration::days(14)),
            ModelStatus::Retired
        );
        l.retire_at = None;
        assert_eq!(
            l.status_at(now + chrono::Duration::days(400)),
            ModelStatus::Deprecated
        );
        // A date on an active model means nothing.
        l.status = ModelStatus::Active;
        l.retire_at = Some(now - chrono::Duration::days(1));
        assert_eq!(l.status_at(now), ModelStatus::Active);
    }

    #[test]
    fn retired_on_prefers_the_retirement_date_then_the_change() {
        let t1 = chrono::Utc::now();
        let t2 = t1 + chrono::Duration::days(3);
        let retired = ModelLifecycle {
            status: ModelStatus::Retired,
            changed_at: Some(t1),
            ..Default::default()
        };
        assert_eq!(retired.retired_on(), Some(t1));
        let scheduled = ModelLifecycle {
            status: ModelStatus::Deprecated,
            retire_at: Some(t2),
            changed_at: Some(t1),
            ..Default::default()
        };
        assert_eq!(scheduled.retired_on(), Some(t2));
        let deprecated = ModelLifecycle {
            status: ModelStatus::Deprecated,
            changed_at: Some(t1),
            ..Default::default()
        };
        assert_eq!(deprecated.retired_on(), None);
    }

    #[test]
    fn payloads_written_before_lifecycle_read_as_active() {
        let mut v = serde_json::to_value(resolved_fixture()).unwrap();
        v.as_object_mut().unwrap().remove("lifecycle");
        let m: ResolvedModel = serde_json::from_value(v).expect("older payload");
        assert_eq!(m.lifecycle.status, ModelStatus::Active);
    }

    #[test]
    fn normalize_variants_trims_dedupes_caps_and_drops_unknown_boons() {
        let raw = vec![
            ModelVariant {
                name: "  glm-5-3-spec ".into(),
                description: " drafted, then verified ".into(),
                boons: vec![
                    "Speculation".into(),
                    "teleportation".into(),
                    "speculation".into(),
                ],
            },
            variant("", &["vision"]),
            variant("glm-5-3-spec", &["vision"]),
            // Case is significant, as for every other model name.
            variant("GLM-5-3-spec", &[]),
        ];
        assert_eq!(
            normalize_variants(&raw),
            vec![
                ModelVariant {
                    name: "glm-5-3-spec".into(),
                    description: "drafted, then verified".into(),
                    boons: vec!["speculation".into()],
                },
                variant("GLM-5-3-spec", &[]),
            ]
        );
        let many: Vec<ModelVariant> = (0..MAX_MODEL_VARIANTS + 3)
            .map(|i| variant(&format!("v{i}"), &[]))
            .collect();
        assert_eq!(normalize_variants(&many).len(), MAX_MODEL_VARIANTS);
    }

    #[test]
    fn union_boons_keeps_the_model_order_and_adds_only_new_ones() {
        let own = vec!["vision".to_string(), "compression".to_string()];
        assert_eq!(
            union_boons(&own, &["speculation".into(), "vision".into()]),
            vec!["vision", "compression", "speculation"]
        );
        // A variant that adds nothing is allowed, and changes nothing.
        assert_eq!(union_boons(&own, &["compression".into()]), own);
        assert_eq!(union_boons(&[], &[]), Vec::<String>::new());
    }

    #[test]
    fn with_variant_adds_boons_and_keeps_everything_capacity_related() {
        let mut parent = resolved_fixture();
        parent.boons = vec!["vision".into()];
        parent.max_in_flight = Some(12);
        parent.capacity_mode = "discovered".into();
        parent.per_replica_max_in_flight = Some(4);
        parent.input_cost_per_token = 0.000_001;
        parent.variants = vec![variant("glm-5-3-spec", &["speculation", "vision"])];

        let served = parent.with_variant("glm-5-3-spec").expect("a variant");
        assert_eq!(served.boons, vec!["vision", "speculation"]);
        // Every other field is the parent's: the pool key (`model_name`), the
        // backend, the prices and the capacity all stay one model's.
        let mut same = served.clone();
        same.boons = parent.boons.clone();
        assert_eq!(same, parent);
        assert_eq!(served.model_name, "glm-5-3");

        // Exact names only: the parent's own name and an alias are not variants.
        assert!(parent.with_variant("glm-5-3").is_none());
        assert!(parent.with_variant("glm-5-3-fp8").is_none());
        assert!(parent.with_variant("GLM-5-3-SPEC").is_none());
    }

    #[test]
    fn payloads_written_before_variants_still_deserialize() {
        // A Redis-cached route from an older build.
        let mut cached = serde_json::to_value(resolved_fixture()).unwrap();
        cached.as_object_mut().unwrap().remove("variants");
        let route: ResolvedModel = serde_json::from_value(cached).unwrap();
        assert!(route.variants.is_empty());

        // A variant written with only a name.
        let v: ModelVariant = serde_json::from_str(r#"{"name":"m-spec"}"#).unwrap();
        assert_eq!(v, variant("m-spec", &[]));

        // A usage record replayed from an older WAL.
        let rec: UsageRecord = serde_json::from_value(serde_json::json!({
            "request_id": Uuid::nil(), "tenant_id": Uuid::nil(), "key_id": Uuid::nil(),
            "model": "m", "admission": "fast", "weight": 1,
            "input_tokens": 1, "output_tokens": 1, "estimated_tokens": 2,
            "queue_wait_ms": 0, "ttft_ms": 0, "total_ms": 0, "status_code": 200,
            "cache_status": "off", "ts_ms": 0,
        }))
        .unwrap();
        assert!(rec.parent_request_id.is_nil());
        assert_eq!(rec.model_variant, "");
    }

    fn resolved_fixture() -> ResolvedModel {
        ResolvedModel {
            model_name: "glm-5-3".into(),
            aliases: vec!["glm-5-3-fp8".into(), "glm-5-3-mxfp4".into()],
            upstream_model: "glm-5-3-mxfp4".into(),
            api_base: "http://upstream/v1".into(),
            quantization: "mxfp4".into(),
            ..Default::default()
        }
    }
}
