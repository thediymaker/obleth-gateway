//! Runtime settings: retention, the assistant, Slurm, energy, alerts, the
//! auto router, every boon, and the per-tenant guardrails and compression
//! policies.

use super::*;

/// Runtime-configurable retention for the raw per-request `usage` ledger.
/// Rows older than `days` are pruned (whole day-partitions dropped) by a
/// background worker; the permanent `usage_daily` rollup is never affected.
/// Persisted in Postgres so changes survive restarts and override the env
/// default.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageRetentionSettings {
    /// Days of raw per-request history to keep. Clamped to a sane floor on use.
    pub days: i64,
}

/// Charo assistant configuration, persisted as a JSON blob in `app_settings`
/// under key `charo_settings`. All fields have serde defaults so a blob written
/// by an older build still loads.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CharoSettings {
    pub enabled: bool,
    /// Model name Charo thinks with. `None` → legacy mode (persona on subject).
    pub brain_model: Option<String>,
    /// Per-tool enable/disable by tool name. Missing key → enabled.
    pub tools_enabled: BTreeMap<String, bool>,
    pub bench_max_concurrency: u32,
    pub bench_max_duration_s: u32,
    pub bench_max_requests: u32,
}

impl Default for CharoSettings {
    fn default() -> Self {
        CharoSettings {
            enabled: true,
            brain_model: None,
            tools_enabled: BTreeMap::new(),
            bench_max_concurrency: 40,
            bench_max_duration_s: 120,
            bench_max_requests: 500,
        }
    }
}

/// System-wide Slurm provisioning settings, editable from the control plane and
/// persisted in Postgres (single `slurm` row in `app_settings`). These hold the
/// slurmrestd connection details the optional `obleth-provisioner` plugin uses;
/// `enabled` is the master switch the dashboard toggles. The JWT is encrypted at
/// rest in the store layer (same envelope cipher as upstream provider keys); it
/// is masked in the public settings API and only returned in full on the
/// provisioner-facing `resolved` route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, ToSchema)]
pub struct SlurmSettings {
    /// Master switch. When false the provisioner idles (no new provisioning).
    #[serde(default)]
    pub enabled: bool,
    /// Base URL of `slurmrestd` (e.g. `http://slurm:6820`).
    #[serde(default)]
    pub slurmrestd_url: String,
    /// `slurmrestd` data-parser version path segment (e.g. `v0.0.43`), or
    /// `auto` (the default; an empty string means the same) to use the newest
    /// version the server offers that obleth supports.
    #[serde(default = "default_slurm_api_version")]
    pub slurmrestd_api_version: String,
    /// Slurm user name sent in `X-SLURM-USER-NAME`.
    #[serde(default)]
    pub slurm_user: String,
    /// Slurm JWT sent in `X-SLURM-USER-TOKEN`. Encrypted at rest by the store.
    #[serde(default)]
    pub slurm_jwt: String,
    /// Operator-supplied compute-node hostname → IP overrides. When the pods
    /// running obleth resolve Slurm node names (`node001`, …) unreliably, these
    /// take DNS out of the loop: the provisioner registers replica endpoints by
    /// IP and probes by IP, so neither the health checker nor the data-plane
    /// proxy depends on per-request name resolution. Empty = pure DNS (the
    /// default). Entries whose host or ip is blank are ignored.
    #[serde(default)]
    pub node_aliases: Vec<NodeAlias>,
    /// Cluster-wide paths and shell setup that recipes fill into the job
    /// scripts they render. Informational to the provisioner: it never
    /// rewrites a stored script from these.
    #[serde(default)]
    pub cluster_defaults: ClusterDefaults,
    /// Hugging Face access token. When set, the provisioner passes it to every
    /// job it submits as `HF_TOKEN` / `HUGGING_FACE_HUB_TOKEN` in the job
    /// environment (never in the script body). Encrypted at rest by the store
    /// and masked in the public settings API, like the JWT.
    #[serde(default)]
    pub hf_token: String,
}

/// Cluster-wide defaults for job scripts, part of `SlurmSettings`. Every field
/// is optional; empty means "not configured".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, ToSchema)]
pub struct ClusterDefaults {
    /// Shared model-weight cache, used as `HF_HOME` in rendered jobs.
    #[serde(default)]
    pub cache_dir: String,
    /// Directory holding container images (e.g. `.sif` files).
    #[serde(default)]
    pub images_dir: String,
    /// Directory for job stdout/stderr files.
    #[serde(default)]
    pub log_dir: String,
    /// Shell lines run at the top of every job (module loads, PATH, ...).
    #[serde(default)]
    pub setup: String,
    /// Engine name → image path or file name (e.g. `vllm` → `vllm.sif`). A
    /// bare file name is relative to `images_dir`.
    #[serde(default)]
    pub images: BTreeMap<String, String>,
}

/// One compute-node hostname → IP override for `SlurmSettings::node_aliases`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct NodeAlias {
    /// Slurm node hostname as it appears in job allocations (e.g. `node001`).
    pub host: String,
    /// The address to use instead — normally an IPv4/IPv6 literal.
    pub ip: String,
}

impl SlurmSettings {
    /// Collapse `node_aliases` into a lookup map, dropping blank/whitespace-only
    /// entries. Later entries win on a duplicate host (last-write-wins), matching
    /// how the UI list reads top-to-bottom.
    pub fn node_alias_map(&self) -> std::collections::HashMap<String, String> {
        self.node_aliases
            .iter()
            .filter_map(|a| {
                let host = a.host.trim();
                let ip = a.ip.trim();
                (!host.is_empty() && !ip.is_empty()).then(|| (host.to_string(), ip.to_string()))
            })
            .collect()
    }
}

fn default_slurm_api_version() -> String {
    crate::slurm::SLURM_API_VERSION_AUTO.to_string()
}

/// System-wide energy & carbon accounting settings, stored as a single
/// `app_settings` row (key `energy`) like the other global settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EnergySettings {
    /// Master switch. When false the power poller no-ops and requests record
    /// zero energy.
    pub enabled: bool,
    /// Base URL of the operator's Prometheus (e.g. `http://prometheus:9090`).
    pub prometheus_url: String,
    /// Operator-supplied PromQL returning one power series (watts) per node.
    /// obleth derives cluster watts via `sum()` and node count via `count()`
    /// of this expression, so any exporter (Habana, DCGM, IPMI) works.
    pub power_query: String,
    /// Operator-supplied PromQL returning one power series (watts) per
    /// model, named by the `model_power_label` label. A model whose name,
    /// capacity Service or upstream model matches a series is charged a
    /// share of that measured power, split across the requests it is
    /// serving cluster-wide, instead of the node slot share.
    pub model_power_query: String,
    /// The label in `model_power_query`'s result that names the model.
    pub model_power_label: String,
    /// Optional PromQL returning how many requests each model's servers are
    /// handling, labelled like `model_power_query`. Set it when clients
    /// other than this gateway share the servers: a model's power is then
    /// split across the larger of the gateway's count and this one.
    pub model_requests_query: String,
    /// Poll cadence in seconds.
    pub poll_interval_secs: u64,
    /// Electricity rate in USD per kWh.
    pub energy_cost_per_kwh: f64,
    /// Grid carbon intensity in grams CO2 per kWh.
    pub carbon_g_per_kwh: f64,
    /// Facility overhead multiplier (PUE). 1.0 = IT power only.
    pub pue: f64,
}

impl Default for EnergySettings {
    fn default() -> Self {
        EnergySettings {
            enabled: false,
            prometheus_url: String::new(),
            power_query: String::new(),
            model_power_query: String::new(),
            model_power_label: "model".to_string(),
            model_requests_query: String::new(),
            poll_interval_secs: 60,
            energy_cost_per_kwh: 0.0,
            carbon_g_per_kwh: 0.0,
            pue: 1.0,
        }
    }
}

impl EnergySettings {
    /// True when the feature is on and the Prometheus target is configured
    /// with at least one of the two queries.
    pub fn active(&self) -> bool {
        self.enabled
            && !self.prometheus_url.trim().is_empty()
            && (!self.power_query.trim().is_empty() || !self.model_power_query.trim().is_empty())
    }

    /// The label naming the model in `model_power_query`'s result.
    pub fn model_label(&self) -> &str {
        match self.model_power_label.trim() {
            "" => "model",
            label => label,
        }
    }
}

/// Runtime-configurable alerting settings, editable from the control plane and
/// persisted in Postgres so they survive restarts. The gateway loads these at
/// boot (falling back to environment defaults) and applies live updates without
/// a restart.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AlertSettings {
    /// Slack incoming-webhook URL. `None` disables Slack delivery.
    pub slack_webhook_url: Option<String>,
    /// SMTP email delivery. `None` disables email alerts.
    pub email: Option<EmailSettings>,
    /// Minimum seconds between repeat alerts for the same dedup key (cooldown).
    pub min_interval_secs: u64,
}

/// SMTP relay configuration for email alerts (universities typically run an
/// internal relay). Credentials are optional for unauthenticated relays.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailSettings {
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// Optional SMTP auth username. `None` for an open/unauthenticated relay.
    #[serde(default)]
    pub username: Option<String>,
    /// Optional SMTP auth password. `None` for an open/unauthenticated relay.
    #[serde(default)]
    pub password: Option<String>,
    /// Envelope/From address (e.g. `obleth-alerts@university.edu`).
    pub from_address: String,
    /// Where alert emails are delivered.
    #[serde(default)]
    pub recipients: Vec<String>,
    /// Use STARTTLS (port 587) when true; plain SMTP (port 25) when false.
    #[serde(default = "default_true")]
    pub starttls: bool,
}

fn default_smtp_port() -> u16 {
    587
}

pub(super) fn default_true() -> bool {
    true
}

impl Default for AlertSettings {
    fn default() -> Self {
        AlertSettings {
            slack_webhook_url: None,
            email: None,
            min_interval_secs: 300,
        }
    }
}

impl AlertSettings {
    /// True when at least one delivery channel is configured.
    pub fn any_channel_enabled(&self) -> bool {
        self.slack_enabled() || self.email_enabled()
    }

    pub fn slack_enabled(&self) -> bool {
        self.slack_webhook_url
            .as_ref()
            .is_some_and(|u| !u.trim().is_empty())
    }

    pub fn email_enabled(&self) -> bool {
        self.email
            .as_ref()
            .is_some_and(|e| !e.smtp_host.trim().is_empty() && !e.recipients.is_empty())
    }
}

/// How a model's per-topic strength level is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TierSource {
    /// Derive from cost rank; an explicit `tag:level` suffix overrides it.
    #[default]
    Hybrid,
    /// Derive from cost rank only; ignore declared suffixes.
    Derived,
    /// Use declared suffixes only; unsuffixed tags are level 1.
    Declared,
}

/// Runtime-editable configuration for the `auto` router's intent classifier
/// and scoring weights. Persisted in `app_settings` under the `auto_router`
/// key so it is editable from the control plane without a restart; seeded
/// from environment (classifier fields) or these defaults (scoring fields)
/// on boot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AutoRouterSettings {
    /// When true, an `auto` request is first sent to the classifier model to
    /// derive intent tags. When false (or unavailable) the router falls back to
    /// cheap heuristics, then capacity/cost scoring.
    pub classifier_enabled: bool,
    /// `model_name` of the registered model used as the classifier "brain"
    /// (e.g. a sub-1B model). `None` disables the model call regardless of
    /// `classifier_enabled`.
    pub classifier_model: Option<String>,
    /// Hard timeout for the classifier call. On timeout the router falls back
    /// to heuristics so an `auto` request is never blocked on the brain.
    pub classifier_timeout_ms: u64,

    /// Relative weight of spare capacity when scoring candidates.
    pub capacity_weight: f64,
    /// Relative weight of cost when scoring candidates.
    pub cost_weight: f64,
    /// Weight of intent-tag match, layered over the capacity/cost base.
    pub tag_weight: f64,
    /// Assumed concurrency ceiling for models with no explicit `max_in_flight`.
    pub default_soft_cap: u32,
    /// 0.0 selects the highest-scoring candidate (deterministic argmax).
    /// Above 0.0, candidates are sampled from a softmax over their scores.
    pub temperature: f64,
    /// When false, the difficulty tier filter is skipped entirely.
    pub difficulty_enabled: bool,
    pub tier_source: TierSource,
    /// When true, a conversation stays on the model that answered its last
    /// turn until there is evidence that model is not doing a good job, and
    /// then moves up a level (see `routing::sticky`).
    pub sticky_enabled: bool,
    /// Model or alias served when a request on the Anthropic Messages surface
    /// names a model the gateway does not know (Claude Code sends Anthropic
    /// model ids). `None` means such requests get `not_found_error`.
    pub messages_default_model: Option<String>,
    /// Model or alias a `/v1/verdicts` request is answered by when it names no
    /// model. `None` means such requests go to `auto`.
    pub verdicts_default_model: Option<String>,
}

impl Default for AutoRouterSettings {
    fn default() -> Self {
        AutoRouterSettings {
            classifier_enabled: false,
            classifier_model: None,
            classifier_timeout_ms: 250,
            capacity_weight: 0.6,
            cost_weight: 0.4,
            tag_weight: 0.5,
            default_soft_cap: 8,
            temperature: 0.0,
            difficulty_enabled: false,
            tier_source: TierSource::default(),
            sticky_enabled: false,
            messages_default_model: None,
            verdicts_default_model: None,
        }
    }
}

impl AutoRouterSettings {
    /// True when the classifier is both enabled and points at a model.
    pub fn classifier_active(&self) -> bool {
        self.classifier_enabled
            && self
                .classifier_model
                .as_ref()
                .is_some_and(|m| !m.trim().is_empty())
    }
}

/// Default prompt sent to the vision model when describing an image for a
/// non-vision model. Operators can override it from the control plane.
///
/// It asks only for what is clearly visible and to say when something can't
/// be read: small describers otherwise guess at fine text and numbers. It
/// doesn't mention the reader being a text-only model, which some describers
/// took to mean that no image had been sent.
pub const DEFAULT_VISION_DESCRIBE_PROMPT: &str = "Describe this image for someone \
who cannot see it: the main subjects and what they are doing, the setting, all \
readable text (verbatim), and any UI elements, code, diagrams, charts or layout. \
State only what you can clearly see. If text or a number is too small or blurry \
to read, say so instead of guessing. Be concise but complete.";

/// The earlier default prompt. A saved setting still equal to it was never
/// edited by an operator, so it follows the current default instead.
const PREVIOUS_VISION_DESCRIBE_PROMPT: &str = "You are assisting a text-only \
model that cannot see images. Describe this image in thorough, faithful detail: \
all visible text (verbatim), UI elements, code, diagrams, charts, layout, and \
any information needed to reason about it. Be concise but complete.";

/// Runtime-editable configuration for the gateway's model "boons" — gateway-side
/// capabilities granted to models that lack them natively. Persisted in
/// `app_settings` under the `boons` key so it is editable from the control plane
/// without a restart.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct BoonSettings {
    /// The vision boon: relay images to a designated vision model for models
    /// that do not natively accept image input.
    pub vision: VisionBoonSettings,
    /// The structured-output boon: enforce `response_format` JSON schemas for
    /// models without native support.
    pub structured_output: StructuredOutputBoonSettings,
    /// The gateway tool loop: inject granted MCP-server tools into chat
    /// requests and execute the model's tool calls at the gateway.
    pub tool_loop: ToolLoopSettings,
    /// The guardrails boon: content policy enforcement via Rust-native scanners
    /// and an optional registered guard model.
    pub guardrails: GuardrailsBoonSettings,
    /// The compression boon: losslessly compact (and, in later phases,
    /// semantically compress) what the model reads before dispatch.
    pub compression: CompressionBoonSettings,
    /// The knowledge boon: ground chat requests on admin-curated institutional
    /// documents retrieved from an in-process vector index.
    pub knowledge: KnowledgeBoonSettings,
    /// The image-generation boon: inject a gateway-executed `generate_image`
    /// tool so a chat model can produce images through a registered image
    /// model.
    pub image_generation: ImageGenerationBoonSettings,
    /// The speculation boon: answer with a fast drafter model when the target
    /// model verifies the draft; escalate to the target otherwise.
    pub speculation: SpeculationBoonSettings,
    /// The web-search boon: inject a gateway-executed `web_search` tool so a
    /// chat model can search the web through a registered `search` route.
    pub web_search: WebSearchBoonSettings,
}

/// Configuration for the vision boon (image-to-text relay).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VisionBoonSettings {
    /// Master switch for the vision boon. When false, requests pass through
    /// unchanged (today's behavior).
    pub enabled: bool,
    /// `model_name` of the registered vision model used to describe images.
    /// `None` disables the relay regardless of `enabled`.
    pub fallback_model: Option<String>,
    /// Instruction sent to the vision model when asking it to describe an image.
    pub describe_prompt: String,
    /// Maximum number of images relayed per request (cost/latency guard).
    /// Images beyond this cap are left untouched.
    pub max_images: u32,
    /// Hard timeout for each describe call, in milliseconds. On timeout the
    /// relay is abandoned and the original request passes through unchanged.
    pub timeout_ms: u64,
    /// How long a description is kept and reused, in seconds. A conversation
    /// sends its earlier images again on every turn; with this, each image is
    /// described once. 0 turns the reuse off.
    pub cache_ttl_secs: u64,
}

impl Default for VisionBoonSettings {
    fn default() -> Self {
        VisionBoonSettings {
            enabled: false,
            fallback_model: None,
            describe_prompt: DEFAULT_VISION_DESCRIBE_PROMPT.to_string(),
            max_images: 6,
            timeout_ms: 30_000,
            cache_ttl_secs: 6 * 60 * 60,
        }
    }
}

impl VisionBoonSettings {
    /// The prompt the describer is sent: the saved one, unless it is still the
    /// previous default, which follows the current default.
    pub fn prompt(&self) -> &str {
        if self.describe_prompt.trim() == PREVIOUS_VISION_DESCRIBE_PROMPT {
            DEFAULT_VISION_DESCRIBE_PROMPT
        } else {
            &self.describe_prompt
        }
    }

    /// True when the vision boon is enabled and points at a fallback model.
    pub fn active(&self) -> bool {
        self.enabled
            && self
                .fallback_model
                .as_ref()
                .is_some_and(|m| !m.trim().is_empty())
    }
}

/// Default description the model reads when deciding whether to call
/// `generate_image`. Deliberately explicit about not describing the result: the
/// tool returns a text receipt, and a model that tries to narrate the image
/// invents details it never saw.
pub const DEFAULT_IMAGE_TOOL_DESCRIPTION: &str =
    "Generate an image from a text description. Call this when the user asks for a picture, \
     drawing, diagram, logo, or any other visual that has to be created rather than described. \
     Write the prompt yourself: state the subject, style, and composition in detail. The \
     generated image is attached to your reply automatically — refer to it, but do not describe \
     its contents.";

/// Hard ceiling on `max_images_per_request`, regardless of what an operator
/// configures (cost, latency, and response-size guard).
pub const IMAGE_GENERATION_MAX_PER_REQUEST: u32 = 4;

/// Configuration for the image-generation boon (gateway-executed
/// `generate_image` tool backed by a registered `image`-type model).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ImageGenerationBoonSettings {
    /// Master switch. When false, no tool is injected and requests pass through
    /// unchanged.
    pub enabled: bool,
    /// `model_name` of the registered `image`-type model that serves
    /// generations. `None` disables the boon regardless of `enabled`.
    pub image_model: Option<String>,
    /// Tool description the model reads when deciding to call the tool.
    pub tool_description: String,
    /// Sizes offered in the tool schema and accepted at execution. A requested
    /// size outside this list is clamped to the first entry.
    pub allowed_sizes: Vec<String>,
    /// Maximum images per tool call, clamped to
    /// [`IMAGE_GENERATION_MAX_PER_REQUEST`].
    pub max_images_per_request: u32,
    /// Hard timeout for one generation call, in milliseconds. On timeout the
    /// model is told the generation failed and answers in prose.
    pub timeout_ms: u64,
}

impl Default for ImageGenerationBoonSettings {
    fn default() -> Self {
        ImageGenerationBoonSettings {
            enabled: false,
            image_model: None,
            tool_description: DEFAULT_IMAGE_TOOL_DESCRIPTION.to_string(),
            allowed_sizes: vec!["512x512".to_string(), "1024x1024".to_string()],
            max_images_per_request: 2,
            timeout_ms: 120_000,
        }
    }
}

impl ImageGenerationBoonSettings {
    /// True when the boon is enabled and points at an image model.
    pub fn active(&self) -> bool {
        self.enabled
            && self
                .image_model
                .as_ref()
                .is_some_and(|m| !m.trim().is_empty())
    }
}

/// Default description the model reads when deciding whether to call
/// `web_search`. It says when NOT to search as plainly as when to: a model told
/// only that it can search tends to search for everything, which costs a round
/// trip on answers it already knew.
pub const DEFAULT_WEB_SEARCH_TOOL_DESCRIPTION: &str =
    "Search the web. Call this when the answer depends on current or external information \
     you are not sure of: recent events, prices, schedules, software versions, documentation, \
     or anything that may have changed since your training. Write a short search-engine \
     query. Answer directly, without searching, when you already know the answer. Cite the \
     URLs of the results you rely on.";

/// Hard ceiling on searches one request may run, regardless of what an
/// operator configures (latency and upstream-load guard).
pub const WEB_SEARCH_MAX_PER_REQUEST: u32 = 8;

/// Hard ceiling on results handed to the model per search (context guard).
pub const WEB_SEARCH_MAX_RESULTS: u32 = 10;

/// Configuration for the web-search boon (gateway-executed `web_search` tool
/// backed by a registered `search`-type route).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WebSearchBoonSettings {
    /// Master switch. When false, no tool is injected and requests pass through
    /// unchanged.
    pub enabled: bool,
    /// `model_name` of the registered `search`-type route that runs the
    /// searches. `None` disables the boon regardless of `enabled`.
    pub search_tool: Option<String>,
    /// Tool description the model reads when deciding to call the tool.
    pub tool_description: String,
    /// Results handed to the model per search, clamped to
    /// [`WEB_SEARCH_MAX_RESULTS`].
    pub max_results: u32,
    /// Searches one request may run across all of its tool turns, clamped to
    /// [`WEB_SEARCH_MAX_PER_REQUEST`].
    pub max_searches_per_request: u32,
    /// Hard timeout for one search, in milliseconds. On timeout the model is
    /// told the search failed and answers from what it knows.
    pub timeout_ms: u64,
}

impl Default for WebSearchBoonSettings {
    fn default() -> Self {
        WebSearchBoonSettings {
            enabled: false,
            search_tool: None,
            tool_description: DEFAULT_WEB_SEARCH_TOOL_DESCRIPTION.to_string(),
            max_results: 5,
            max_searches_per_request: 3,
            timeout_ms: 15_000,
        }
    }
}

impl WebSearchBoonSettings {
    /// True when the boon is enabled and points at a search route.
    pub fn active(&self) -> bool {
        self.enabled
            && self
                .search_tool
                .as_ref()
                .is_some_and(|m| !m.trim().is_empty())
    }
}

fn default_spec_agree_min() -> f64 {
    0.5
}

fn default_spec_lp_min() -> f64 {
    -1.0
}

/// Configuration for the speculation boon (draft-verify cascade).
///
/// The drafter writes a candidate answer; the verifier — a deployment of the
/// TARGET model family whose backend supports `prompt_logprobs` — scores every
/// draft token in one prefill (~50x cheaper than decoding them). The gate is a
/// pair of floors over that scoring: `agree_min` on the fraction of draft
/// tokens the verifier would itself have picked (rank 1), `lp_min` on the mean
/// log-probability. Streaming uses a three-zone gate: at or above the gate a
/// chunk is released to the client; below the `abort_*` floors the draft is
/// abandoned and the target model answers; in between the decision is deferred
/// while the draft grows (a draft's stylistic opening scores lowest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SpeculationBoonSettings {
    /// Master switch. When false, requests pass through unchanged.
    pub enabled: bool,
    /// `model_name` of the registered chat model that drafts answers. Must be
    /// several times faster than the target for the boon to pay off. `None`
    /// disables the boon regardless of `enabled`.
    pub draft_model: Option<String>,
    /// Ship floor: minimum fraction of draft tokens the verifier ranks #1.
    pub agree_min: f64,
    /// Ship floor: minimum mean log-probability over draft tokens.
    pub lp_min: f64,
    /// Abort floor (streaming): below this agreement, stop drafting and
    /// escalate immediately instead of deferring.
    pub abort_agree: f64,
    /// Abort floor (streaming): below this mean log-probability, stop drafting
    /// and escalate immediately instead of deferring.
    pub abort_lp: f64,
    /// Draft tokens accumulated before the FIRST verification. Small, so a
    /// passing draft starts streaming early and a failing one aborts cheaply.
    pub first_chunk_tokens: u32,
    /// Draft tokens between subsequent verifications.
    pub chunk_tokens: u32,
    /// Defer patience: if nothing has been released by this many draft tokens
    /// and the cumulative stats still sit below the gate, escalate — such
    /// drafts almost never recover, and every deferred token is added latency
    /// before the target model starts over.
    pub decide_by_tokens: u32,
    /// Cap on the drafter's `max_tokens` (a reasoning drafter can burn an
    /// unbounded budget producing nothing shippable).
    pub max_draft_tokens: u32,
    /// Milliseconds between released draft deltas, so verified spans play back
    /// as a continuous stream instead of bursts. 0 releases each span at once.
    pub pace_ms: u64,
    /// `chat_template_kwargs` sent with DRAFT calls only (e.g.
    /// `{"reasoning": false}` on drafters whose template can skip thinking —
    /// measured both faster and more reliable than letting the drafter reason).
    /// Never forwarded to the verifier or the target.
    pub draft_chat_template_kwargs: Option<serde_json::Value>,
    /// Wall-clock budget in milliseconds for the pre-release phase (drafting +
    /// verification before anything is sent to the client). On timeout the
    /// request falls through to the target model unchanged.
    pub timeout_ms: u64,
    /// `model_name` of a registered tiny model that classifies each request
    /// into one of the `category_gates` tags before drafting (the same
    /// mechanism as the auto-router's brain, with this boon's own vocabulary).
    /// `None` disables classification: the default gate applies to everything.
    pub classify_model: Option<String>,
    /// Per-category gate overrides. Calibration shows the gate's precision is
    /// strongly category-dependent — and that some categories should never
    /// ship a draft at all (`speculate: false` abstains before the draft is
    /// even requested, costing nothing).
    pub category_gates: Vec<SpeculationCategoryGate>,
    /// What happens when the request's category has no `category_gates` entry
    /// (including when classification is off or failed): `true` applies the
    /// default `agree_min`/`lp_min` gate, `false` abstains (the target model
    /// answers directly).
    pub unlisted_categories_speculate: bool,
    /// Fleet rule for locating a model's scoring endpoint when the model does
    /// not set its own `verify_api_base`: a URL with `{upstream}` and/or
    /// `{model}` placeholders (e.g.
    /// `http://{upstream}.serving.svc.cluster.local:8000/v1`). Only set this
    /// once the fleet's backends can survive `prompt_logprobs` scoring —
    /// pointing it at unpatched pods kills them. Empty = models without their
    /// own endpoint cannot speculate.
    pub verify_url_template: String,
    /// Per-call bound on a verifier call (tokenize, score), in milliseconds.
    /// Before anything is released the pre-release budget also applies; after
    /// the commit this is what ends a stalled scorer, so the committed stream
    /// ends instead of going silent.
    pub verify_timeout_ms: u64,
    /// Largest verifier reply read, in bytes. A `prompt_logprobs` answer runs
    /// to a few hundred bytes per prompt token, so a long prompt is megabytes.
    pub verify_reply_max_bytes: usize,
}

fn default_spec_gate_speculate() -> bool {
    true
}

/// One per-category gate for the speculation boon. Thresholds default to the
/// global floors; `speculate: false` excludes the category entirely.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpeculationCategoryGate {
    /// The classifier tag this gate applies to.
    pub tag: String,
    /// False = never draft for this category.
    #[serde(default = "default_spec_gate_speculate")]
    pub speculate: bool,
    /// Ship floor override for this category.
    #[serde(default = "default_spec_agree_min")]
    pub agree_min: f64,
    /// Ship floor override for this category.
    #[serde(default = "default_spec_lp_min")]
    pub lp_min: f64,
}

impl Default for SpeculationBoonSettings {
    fn default() -> Self {
        SpeculationBoonSettings {
            enabled: false,
            draft_model: None,
            agree_min: 0.5,
            lp_min: -1.0,
            abort_agree: 0.45,
            abort_lp: -1.6,
            first_chunk_tokens: 80,
            chunk_tokens: 250,
            decide_by_tokens: 450,
            max_draft_tokens: 2048,
            pace_ms: 9,
            draft_chat_template_kwargs: None,
            timeout_ms: 45_000,
            classify_model: None,
            category_gates: Vec::new(),
            unlisted_categories_speculate: true,
            verify_url_template: String::new(),
            verify_timeout_ms: 30_000,
            verify_reply_max_bytes: 64 * 1024 * 1024,
        }
    }
}

impl SpeculationBoonSettings {
    /// True when the boon is switched on. Everything else is per target: the
    /// drafter comes from the model's own `draft_model` (this `draft_model`
    /// here is only the fleet default) and verification uses the model's
    /// `verify_api_base`.
    pub fn active(&self) -> bool {
        self.enabled
    }
}

/// Maximum repair attempts an operator may configure for the structured-output
/// boon (cost guard).
pub const STRUCTURED_OUTPUT_MAX_REPAIR_ATTEMPTS: u32 = 3;

/// Configuration for the structured-output boon (JSON-schema enforcement).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StructuredOutputBoonSettings {
    /// Master switch for the structured-output boon. When false, requests pass
    /// through unchanged.
    pub enabled: bool,
    /// `model_name` of the registered model used to repair invalid JSON.
    /// `None` re-prompts the request's own model instead.
    pub fixer_model: Option<String>,
    /// Maximum repair calls per request when validation fails
    /// (clamped to [`STRUCTURED_OUTPUT_MAX_REPAIR_ATTEMPTS`]).
    pub max_repair_attempts: u32,
    /// Hard timeout for each repair call, in milliseconds.
    pub timeout_ms: u64,
}

impl Default for StructuredOutputBoonSettings {
    fn default() -> Self {
        StructuredOutputBoonSettings {
            enabled: false,
            fixer_model: None,
            max_repair_attempts: 1,
            timeout_ms: 30_000,
        }
    }
}

impl StructuredOutputBoonSettings {
    /// True when the structured-output boon is enabled.
    pub fn active(&self) -> bool {
        self.enabled
    }
}

/// Default system nudge injected alongside granted tools so under-eager models
/// reach for them. Names the capability explicitly ("you can call tools") and
/// the situations that warrant a call, without forcing tool use on every turn.
pub fn default_tool_loop_nudge() -> String {
    "You have tools available in this conversation and can call them. \
When a question needs current, external, or factual information you are not \
certain of — news, prices, weather, software versions, documentation, recent \
events, anything after your knowledge cutoff — call the appropriate tool to \
look it up before answering, rather than guessing or saying you cannot. Use \
your own knowledge directly for everything else."
        .to_string()
}

/// Maximum tool-loop turns an operator may configure (cost/latency guard).
pub const TOOL_LOOP_MAX_TURNS: u32 = 8;

/// Largest `ToolLoopSettings::deadline_secs` an operator may configure (one
/// hour). Also the fallback budget when a stored value cannot be represented
/// as a deadline.
pub const TOOL_LOOP_MAX_DEADLINE_SECS: u64 = 3_600;

/// Maximum gateway tool calls executed from one model turn. Each excess call
/// gets an error tool result instead, so a single reply cannot fan out an
/// unbounded number of MCP or image calls.
pub const TOOL_LOOP_MAX_CALLS_PER_TURN: usize = 16;

/// Configuration for the gateway tool loop: when a model is granted access to
/// registered MCP servers (`ModelRoute::tool_servers`), the gateway injects
/// the servers' tools into plain chat requests, executes the model's tool
/// calls against the MCP upstream, and loops until the model produces a final
/// answer. Clients that send their own `tools` are left untouched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ToolLoopSettings {
    /// Master switch for the gateway tool loop. When false, granted tool
    /// servers are ignored and requests pass through unchanged.
    pub enabled: bool,
    /// Maximum model round trips per request before the loop stops and the
    /// last completion is returned as-is (clamped to [`TOOL_LOOP_MAX_TURNS`]).
    pub max_turns: u32,
    /// Hard timeout for each MCP tool execution, in milliseconds.
    pub tool_timeout_ms: u64,
    /// System instruction injected alongside the granted tools so the model
    /// actually calls them when a question needs external information. Tunable
    /// per deployment; injected only for plain chat clients (clients that bring
    /// their own `tools` manage their own tool-use policy and are left
    /// untouched). An empty string disables the nudge.
    pub nudge: String,
    /// Wall-clock budget for the whole loop of one request (every tool call
    /// and follow-up turn), in seconds. Per-call timeouts alone stack up to
    /// `max_turns` x calls x timeout of held upstream capacity; once this
    /// elapses the loop dispatches nothing further.
    pub deadline_secs: u64,
}

impl Default for ToolLoopSettings {
    fn default() -> Self {
        ToolLoopSettings {
            enabled: false,
            max_turns: 4,
            tool_timeout_ms: 30_000,
            nudge: default_tool_loop_nudge(),
            deadline_secs: 300,
        }
    }
}

impl ToolLoopSettings {
    /// True when the gateway tool loop is enabled.
    pub fn active(&self) -> bool {
        self.enabled
    }
}

/// Global tuning for the guardrails boon. There is intentionally no master
/// `enabled` flag: guardrails are enabled per-tenant by the presence of a
/// [`GuardrailsPolicy`], so a configured policy always takes effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(default)]
pub struct GuardrailsBoonSettings {
    /// Timeout for tier-2 guard model calls (ms). Does not apply to tier-1 scanners.
    pub timeout_ms: u64,
}

impl Default for GuardrailsBoonSettings {
    fn default() -> Self {
        Self { timeout_ms: 5_000 }
    }
}

/// Configuration for the compression boon: lossless structural/code compaction
/// plus deterministic (model-free) lossy text compaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CompressionBoonSettings {
    /// Master switch. When false, requests pass through unchanged.
    pub enabled: bool,
    /// Skip any message segment whose estimated token count is below this
    /// floor (overhead would exceed savings).
    pub min_tokens: u32,
    /// Cap on how many segments the lossless pass compacts per request. The
    /// pass also stops after reading 8 MB of segment text.
    pub max_segments: u32,
    /// Redis TTL for stashed originals (seconds); tuned to a session lifetime.
    pub original_ttl_secs: u64,
    /// Segments each reversible text pass (dedup, log, lossy) may replace
    /// per request; distinct from `max_segments`, which caps lossless
    /// compaction.
    pub max_lossy_segments: u32,
    /// Conservative, opt-in whitespace normalization of fenced code segments
    /// (strip trailing whitespace, collapse blank-line runs). Off by default.
    pub code_compaction: bool,
    /// Cross-turn exact-duplicate dedup default. A per-tenant policy overrides
    /// this; tenants with no policy inherit it. Off by default.
    pub dedup: bool,
    /// Near-lossless log template-collapse default (repeated log lines → one
    /// representative line + `(×N)`). A per-tenant policy overrides this;
    /// tenants with no policy inherit it. Off by default.
    pub compact_logs: bool,
    /// Lossy text compaction default (salience-based prose line/sentence drop;
    /// uses the neural sidecar when deployed). A per-tenant policy overrides
    /// this; tenants with no policy inherit it. Off by default.
    pub allow_lossy: bool,
    /// Fraction of sentences the compressor sidecar selects to keep during the
    /// neural extractive pass. Range `(0.0, 1.0]`; defaults to 0.5.
    pub neural_keep_ratio: f32,
}

impl Default for CompressionBoonSettings {
    fn default() -> Self {
        CompressionBoonSettings {
            enabled: false,
            min_tokens: 128,
            max_segments: 64,
            original_ttl_secs: 3_600,
            max_lossy_segments: 4,
            code_compaction: false,
            dedup: false,
            compact_logs: false,
            allow_lossy: false,
            neural_keep_ratio: 0.5,
        }
    }
}

impl CompressionBoonSettings {
    /// True when the boon is enabled. (All compaction is deterministic; no helper.)
    pub fn active(&self) -> bool {
        self.enabled
    }
}

/// Configuration for the knowledge boon (institutional RAG).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct KnowledgeBoonSettings {
    /// Master switch. False leaves every request untouched.
    pub enabled: bool,
    /// Chunks considered per request before thresholding.
    pub top_k: u32,
    /// Cosine floor; a chunk below this is dropped even if it is the best match.
    pub min_score: f32,
    /// Ceiling on injected tokens, before the context-window clamp.
    pub max_context_tokens: u32,
    /// Hard bound on the request-path query-embedding call.
    pub embed_timeout_ms: u64,
    /// Query-vector cache lifetime in Redis.
    pub query_cache_ttl_s: u64,
    /// How many trailing user messages form the retrieval query.
    pub query_turns: u32,
    /// Per-file upload cap, rejected before parsing.
    pub max_upload_bytes: i64,
    /// Per-collection chunk cap; indexing refuses past it.
    pub max_chunks_per_collection: i64,
    /// Chunks per upstream embedding call during indexing.
    pub index_batch_size: u32,
    /// Timeout for indexing-time embedding calls (looser than the hot path).
    pub index_timeout_ms: u64,
    /// How long a claimed document may sit in `indexing` before another worker
    /// may reclaim it. Generous on purpose: it must exceed the worst-case
    /// embedding time for a large document, or a slow document gets embedded
    /// twice.
    pub index_stale_after_secs: i64,
    /// Write retrieved chunk *text* into spans. Costs roughly 20x the default
    /// tracing tier, so it is opt-in and surfaced with a size warning.
    pub debug_snapshot: bool,
}

impl Default for KnowledgeBoonSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            top_k: 5,
            min_score: 0.35,
            max_context_tokens: 1500,
            embed_timeout_ms: 1500,
            query_cache_ttl_s: 600,
            query_turns: 2,
            max_upload_bytes: 10 * 1024 * 1024,
            max_chunks_per_collection: 100_000,
            index_batch_size: 32,
            index_timeout_ms: 30_000,
            index_stale_after_secs: 1_800,
            debug_snapshot: false,
        }
    }
}

impl KnowledgeBoonSettings {
    /// True when the boon may run at all.
    pub fn active(&self) -> bool {
        self.enabled
    }
}

/// Per-tenant guardrails policy. Stored as JSONB on `tenants.guardrails_policy`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct GuardrailsPolicy {
    /// Action taken when any scanner flags content.
    pub action: GuardrailsAction,
    /// Scanners to run on the incoming request body.
    /// Valid values: "prompt_injection", "pii", "ban_keywords", "harm"
    #[serde(default)]
    pub input_scanners: Vec<String>,
    /// Scanners to run on the model's response.
    /// Valid values: "pii", "ban_keywords", "harm"
    #[serde(default)]
    pub output_scanners: Vec<String>,
    /// Registered model name for the tier-2 `harm` scanner.
    #[serde(default)]
    pub guard_model: Option<String>,
    /// Tenant-specific keyword list for the `ban_keywords` scanner.
    #[serde(default)]
    pub ban_keywords: Vec<String>,
    /// When true, scanner errors pass through unchanged.
    #[serde(default = "default_true")]
    pub fail_open: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GuardrailsAction {
    Block,
    Redact,
    LogOnly,
}

/// Per-tenant compression policy. Stored as JSONB on `tenants.compression_policy`.
///
/// A `None` policy on a tenant means "follow the global default" (eligible models
/// are compressed). Each field independently toggles a compression piece for the tenant:
/// `enabled` (master on/off; lossless JSON compaction is the safe baseline under it),
/// `code_compaction` (conservative code whitespace), `dedup` (cross-turn exact-duplicate dedup),
/// `allow_lossy` (lossy semantic summarization).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct CompressionPolicy {
    /// Tenant opt-out switch for the compression boon. When false, no
    /// compression (lossless or lossy) runs for this tenant's keys, even on
    /// models that are granted the boon.
    #[serde(default)]
    pub enabled: bool,
    /// Tenant toggle for conservative code-whitespace compaction (lossless-ish).
    #[serde(default)]
    pub code_compaction: bool,
    /// Tenant toggle for cross-turn exact-duplicate dedup (reversible).
    #[serde(default)]
    pub dedup: bool,
    /// Tenant toggle for log template-collapse: repeated, structurally-identical
    /// log lines are collapsed to a single representative `(×N)`, while error and
    /// warning lines are kept verbatim. Reversible (the original is stashed for
    /// `retrieve_original`) and far safer than general prose summarization, so it
    /// is a separate opt-in from `allow_lossy`.
    #[serde(default)]
    pub compact_logs: bool,
    /// Whether the tenant accepts lossy semantic compression. Consumed by the
    /// lossy compressor in Phase B2; ignored by Phase A/B1 lossless compaction.
    #[serde(default)]
    pub allow_lossy: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each runtime settings struct reads an empty object as its `Default`:
    /// `impl Default` is the one source of every default, and a blob written
    /// before a field existed loads with that field's default.
    #[test]
    fn an_empty_settings_object_reads_as_the_default() {
        fn check<T: serde::de::DeserializeOwned + Default + PartialEq + std::fmt::Debug>() {
            let read: T = serde_json::from_str("{}").unwrap();
            assert_eq!(read, T::default(), "{}", std::any::type_name::<T>());
        }
        check::<CharoSettings>();
        check::<EnergySettings>();
        check::<AlertSettings>();
        check::<AutoRouterSettings>();
        check::<BoonSettings>();
        check::<VisionBoonSettings>();
        check::<ImageGenerationBoonSettings>();
        check::<WebSearchBoonSettings>();
        check::<SpeculationBoonSettings>();
        check::<StructuredOutputBoonSettings>();
        check::<ToolLoopSettings>();
        check::<GuardrailsBoonSettings>();
        check::<CompressionBoonSettings>();
        check::<KnowledgeBoonSettings>();
    }

    #[test]
    fn auto_router_defaults_match_router_constants() {
        let s = AutoRouterSettings::default();
        assert_eq!(s.capacity_weight, 0.6);
        assert_eq!(s.cost_weight, 0.4);
        assert_eq!(s.tag_weight, 0.5);
        assert_eq!(s.default_soft_cap, 8);
        assert_eq!(s.temperature, 0.0);
        assert!(!s.difficulty_enabled);
        assert_eq!(s.tier_source, TierSource::Hybrid);
    }

    #[test]
    fn auto_router_row_without_new_fields_still_parses() {
        // A row written before this feature shipped.
        let json =
            r#"{"classifier_enabled":true,"classifier_model":"brain","classifier_timeout_ms":250}"#;
        let s: AutoRouterSettings = serde_json::from_str(json).unwrap();
        assert!(s.classifier_enabled);
        assert_eq!(s.classifier_model.as_deref(), Some("brain"));
        // New fields fall back to the constants, not to zero.
        assert_eq!(s.capacity_weight, 0.6);
        assert_eq!(s.default_soft_cap, 8);
        assert!(!s.difficulty_enabled);
    }

    #[test]
    fn slurm_settings_defaults_have_no_node_aliases() {
        // A pre-existing `slurm` app_settings blob (no node_aliases key) must
        // still deserialize, with an empty override map.
        let s: SlurmSettings = serde_json::from_str("{}").expect("legacy blob deserializes");
        assert!(s.node_aliases.is_empty());
        assert!(s.node_alias_map().is_empty());
    }

    #[test]
    fn slurm_settings_legacy_blob_has_empty_cluster_defaults_and_no_hf_token() {
        let s: SlurmSettings =
            serde_json::from_str(r#"{"enabled":true,"slurm_user":"svc"}"#).expect("parses");
        assert_eq!(s.cluster_defaults, ClusterDefaults::default());
        assert!(s.hf_token.is_empty());
        // A partial cluster_defaults object fills the rest with defaults.
        let s: SlurmSettings =
            serde_json::from_str(r#"{"cluster_defaults":{"images":{"vllm":"vllm.sif"}}}"#)
                .expect("parses");
        assert_eq!(
            s.cluster_defaults.images.get("vllm").map(String::as_str),
            Some("vllm.sif")
        );
        assert!(s.cluster_defaults.cache_dir.is_empty());
    }

    #[test]
    fn node_alias_map_drops_blanks_and_takes_last_on_dupes() {
        let s = SlurmSettings {
            node_aliases: vec![
                NodeAlias {
                    host: " node001 ".into(),
                    ip: " 10.0.0.1 ".into(),
                }, // trimmed
                NodeAlias {
                    host: "".into(),
                    ip: "10.0.0.9".into(),
                }, // blank host → drop
                NodeAlias {
                    host: "node002".into(),
                    ip: "".into(),
                }, // blank ip → drop
                NodeAlias {
                    host: "node001".into(),
                    ip: "10.0.0.2".into(),
                }, // dupe → wins
            ],
            ..Default::default()
        };
        let map = s.node_alias_map();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("node001").map(String::as_str), Some("10.0.0.2"));
    }

    #[test]
    fn vision_prompt_left_at_the_earlier_default_follows_the_new_one() {
        let mut v = VisionBoonSettings {
            describe_prompt: PREVIOUS_VISION_DESCRIBE_PROMPT.to_string(),
            ..Default::default()
        };
        assert_eq!(v.prompt(), DEFAULT_VISION_DESCRIBE_PROMPT);
        v.describe_prompt = "Describe the chart only.".into();
        assert_eq!(v.prompt(), "Describe the chart only.");
    }

    #[test]
    fn vision_settings_saved_before_the_cache_reuse_descriptions() {
        let v: VisionBoonSettings =
            serde_json::from_str(r#"{"enabled":true,"fallback_model":"m"}"#).unwrap();
        assert_eq!(v.cache_ttl_secs, 6 * 60 * 60);
    }

    #[test]
    fn compression_code_compaction_defaults_false() {
        let c: CompressionBoonSettings = serde_json::from_str("{}").unwrap();
        assert!(!c.code_compaction);
    }

    #[test]
    fn boon_settings_backward_compat_deserialize() {
        // A stored blob from before the structured_output boon existed must
        // deserialize with the new fields defaulted off. A retired `tools` blob
        // is ignored (the field no longer exists).
        let old = r#"{"vision":{"enabled":true,"fallback_model":"llava","describe_prompt":"p","max_images":4,"timeout_ms":1000},"tools":{"enabled":true,"max_tools":8}}"#;
        let settings: BoonSettings = serde_json::from_str(old).expect("old blob deserializes");
        assert!(settings.vision.enabled);
        assert_eq!(settings.vision.fallback_model.as_deref(), Some("llava"));
        assert!(!settings.structured_output.enabled);
        assert_eq!(settings.structured_output.max_repair_attempts, 1);
        assert_eq!(settings.structured_output.timeout_ms, 30_000);
        assert_eq!(settings.guardrails.timeout_ms, 5_000);
        assert!(!settings.speculation.enabled);
        assert!(!settings.speculation.active());
        assert_eq!(settings.speculation.agree_min, 0.5);
        assert_eq!(settings.speculation.first_chunk_tokens, 80);
    }

    #[test]
    fn speculation_settings_active_is_just_the_switch() {
        let mut s = SpeculationBoonSettings::default();
        assert!(!s.active(), "disabled by default");
        s.enabled = true;
        assert!(
            s.active(),
            "drafter and verifier both resolve per target model; no global requirement"
        );
    }

    #[test]
    fn boon_active_flags() {
        let mut structured = StructuredOutputBoonSettings::default();
        assert!(!structured.active());
        structured.enabled = true;
        assert!(structured.active());
    }

    #[test]
    fn guardrails_settings_default_timeout() {
        let s = GuardrailsBoonSettings::default();
        assert_eq!(s.timeout_ms, 5_000);
    }

    #[test]
    fn guardrails_policy_deserializes() {
        let json = r#"{
            "action": "redact",
            "input_scanners": ["pii", "prompt_injection"],
            "output_scanners": ["pii"],
            "guard_model": null,
            "ban_keywords": [],
            "fail_open": true
        }"#;
        let policy: GuardrailsPolicy = serde_json::from_str(json).unwrap();
        assert!(matches!(policy.action, GuardrailsAction::Redact));
        assert_eq!(policy.input_scanners, vec!["pii", "prompt_injection"]);
        assert!(policy.fail_open);
    }

    #[test]
    fn guardrails_policy_defaults_fail_open() {
        let json = r#"{"action":"block","input_scanners":["harm"],"output_scanners":[]}"#;
        let policy: GuardrailsPolicy = serde_json::from_str(json).unwrap();
        assert!(policy.fail_open);
    }

    #[test]
    fn boon_settings_includes_guardrails() {
        // A legacy `enabled` field is ignored; timeout still parses.
        let json = r#"{"guardrails":{"enabled":true,"timeout_ms":2000}}"#;
        let settings: BoonSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.guardrails.timeout_ms, 2_000);
    }

    #[test]
    fn compression_settings_default_is_disabled() {
        let c = CompressionBoonSettings::default();
        assert!(!c.enabled);
        assert!(!c.active());
        assert_eq!(c.min_tokens, 128);
        assert_eq!(c.max_segments, 64);
    }

    #[test]
    fn compression_settings_active_when_enabled() {
        let c = CompressionBoonSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(c.active());
    }

    #[test]
    fn boon_settings_default_has_compression() {
        let b = BoonSettings::default();
        assert!(!b.compression.active());
    }

    #[test]
    fn compression_settings_deserialize_with_missing_fields() {
        // All fields are #[serde(default)] so an empty object must work.
        let c: CompressionBoonSettings = serde_json::from_str("{}").unwrap();
        assert!(!c.enabled);
        assert_eq!(c.min_tokens, 128);
    }

    #[test]
    fn compression_settings_lossy_defaults() {
        let c: CompressionBoonSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(c.original_ttl_secs, 3_600);
        assert_eq!(c.max_lossy_segments, 4);
    }

    #[test]
    fn compression_policy_defaults_from_empty_json() {
        // An empty policy object means "configured, default off, no lossy".
        let p: CompressionPolicy = serde_json::from_str("{}").unwrap();
        assert!(!p.enabled);
        assert!(!p.allow_lossy);
    }

    #[test]
    fn compression_policy_round_trips() {
        let p = CompressionPolicy {
            enabled: true,
            code_compaction: false,
            dedup: false,
            compact_logs: false,
            allow_lossy: true,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: CompressionPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn compression_policy_new_fields_default_false() {
        // Old 2-field shape still deserializes; new piece-flags default off.
        let p: CompressionPolicy =
            serde_json::from_str(r#"{"enabled":true,"allow_lossy":true}"#).unwrap();
        assert!(p.enabled);
        assert!(p.allow_lossy);
        assert!(!p.code_compaction);
        assert!(!p.dedup);
    }

    #[test]
    fn compression_policy_full_round_trips() {
        let p = CompressionPolicy {
            enabled: true,
            code_compaction: true,
            dedup: true,
            compact_logs: true,
            allow_lossy: false,
        };
        let back: CompressionPolicy =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn energy_settings_defaults() {
        let s: EnergySettings = serde_json::from_str("{}").unwrap();
        assert!(!s.enabled);
        assert_eq!(s.poll_interval_secs, 60);
        assert_eq!(s.pue, 1.0);
        assert!(!s.active());
        let active = EnergySettings {
            enabled: true,
            prometheus_url: "http://prom:9090".into(),
            power_query: "node_power_watts".into(),
            ..Default::default()
        };
        assert!(active.active());
        assert_eq!(s.model_label(), "model");
        let measured = EnergySettings {
            enabled: true,
            prometheus_url: "http://prom:9090".into(),
            model_power_query: "model_power_watts".into(),
            model_power_label: " ".into(),
            ..Default::default()
        };
        assert!(measured.active(), "a model query alone is enough");
        assert_eq!(
            measured.model_label(),
            "model",
            "a blank label reads as the default"
        );
    }

    #[test]
    fn charo_settings_defaults() {
        let s = CharoSettings::default();
        assert!(s.enabled);
        assert_eq!(s.brain_model, None);
        assert!(s.tools_enabled.is_empty());
        assert_eq!(s.bench_max_concurrency, 40);
        assert_eq!(s.bench_max_duration_s, 120);
        assert_eq!(s.bench_max_requests, 500);
    }

    #[test]
    fn charo_settings_deserializes_partial_json() {
        // A stored blob written before a field existed still loads (serde defaults).
        let s: CharoSettings = serde_json::from_str(r#"{"brain_model":"llama-3"}"#).unwrap();
        assert_eq!(s.brain_model.as_deref(), Some("llama-3"));
        assert!(s.enabled); // default
        assert_eq!(s.bench_max_concurrency, 40); // default
    }

    #[test]
    fn knowledge_settings_default_to_off() {
        // Existing installs must be unaffected until an admin opts in.
        let s = KnowledgeBoonSettings::default();
        assert!(!s.enabled);
        assert!(!s.active());
        assert!(!s.debug_snapshot, "the expensive tracing tier is opt-in");
    }

    #[test]
    fn knowledge_settings_have_usable_defaults_once_enabled() {
        let s = KnowledgeBoonSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(s.active());
        assert_eq!(s.top_k, 5);
        assert_eq!(s.max_context_tokens, 1500);
        assert_eq!(s.query_turns, 2);
    }

    #[test]
    fn boon_settings_without_knowledge_still_deserialize() {
        // Settings rows written by an older build have no `knowledge` key.
        let old = r#"{"vision":{"enabled":true}}"#;
        let s: BoonSettings = serde_json::from_str(old).expect("deserialize");
        assert!(!s.knowledge.enabled);
    }

    #[test]
    fn image_generation_defaults_are_off_and_bounded() {
        let s = ImageGenerationBoonSettings::default();
        assert!(!s.enabled);
        assert!(s.image_model.is_none());
        assert_eq!(
            s.allowed_sizes,
            vec!["512x512".to_string(), "1024x1024".to_string()]
        );
        assert_eq!(s.max_images_per_request, 2);
        assert_eq!(s.timeout_ms, 120_000);
        assert!(!s.tool_description.trim().is_empty());
        assert!(!s.active());
    }

    #[test]
    fn image_generation_active_requires_enabled_and_a_model() {
        let s = ImageGenerationBoonSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(!s.active(), "enabled with no model is inactive");
        let s = ImageGenerationBoonSettings {
            enabled: true,
            image_model: Some("   ".to_string()),
            ..Default::default()
        };
        assert!(!s.active(), "a blank model name is inactive");
        let s = ImageGenerationBoonSettings {
            enabled: true,
            image_model: Some("sdxl".to_string()),
            ..Default::default()
        };
        assert!(s.active());
        let s = ImageGenerationBoonSettings {
            image_model: Some("sdxl".to_string()),
            ..Default::default()
        };
        assert!(!s.active());
    }

    #[test]
    fn boon_settings_row_from_an_older_build_still_parses() {
        // A `boons` row written before this field existed has no
        // `image_generation` key at all; it must deserialize to the default.
        let older = serde_json::json!({ "vision": { "enabled": true } });
        let parsed: BoonSettings = serde_json::from_value(older).expect("older row parses");
        assert!(parsed.vision.enabled);
        assert!(!parsed.image_generation.enabled);
        assert_eq!(parsed.image_generation.max_images_per_request, 2);
    }

    #[test]
    fn web_search_defaults_are_off_and_bounded() {
        let s = WebSearchBoonSettings::default();
        assert!(!s.enabled);
        assert!(s.search_tool.is_none());
        assert_eq!(s.max_results, 5);
        assert_eq!(s.max_searches_per_request, 3);
        assert_eq!(s.timeout_ms, 15_000);
        assert!(s.max_results <= WEB_SEARCH_MAX_RESULTS);
        assert!(s.max_searches_per_request <= WEB_SEARCH_MAX_PER_REQUEST);
        assert!(!s.tool_description.trim().is_empty());
        assert!(!s.active());
    }

    #[test]
    fn web_search_active_requires_enabled_and_a_search_tool() {
        let enabled_only = WebSearchBoonSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(!enabled_only.active());
        let blank = WebSearchBoonSettings {
            enabled: true,
            search_tool: Some("  ".to_string()),
            ..Default::default()
        };
        assert!(!blank.active());
        let tool_only = WebSearchBoonSettings {
            search_tool: Some("web".to_string()),
            ..Default::default()
        };
        assert!(!tool_only.active());
        let on = WebSearchBoonSettings {
            enabled: true,
            search_tool: Some("web".to_string()),
            ..Default::default()
        };
        assert!(on.active());
    }

    #[test]
    fn a_boons_row_without_web_search_parses_to_the_default() {
        let older = serde_json::json!({ "image_generation": { "enabled": true } });
        let parsed: BoonSettings = serde_json::from_value(older).expect("older row parses");
        assert!(parsed.image_generation.enabled);
        assert_eq!(parsed.web_search, WebSearchBoonSettings::default());
    }

    #[test]
    fn auto_router_row_without_messages_default_model_still_parses() {
        let v: AutoRouterSettings =
            serde_json::from_str(r#"{"classifier_enabled":false}"#).unwrap();
        assert_eq!(v.messages_default_model, None);
        assert_eq!(v.verdicts_default_model, None);
    }
}
