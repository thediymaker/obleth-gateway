// Server-side client for the obleth Management API.

// React's request-scoped memo: no-arg settings getters are wrapped in
// `reactCache` so a layout and page reading the same setting in one render
// tree cost one admin round-trip. Freshness is unchanged — the memo does not
// outlive the request.
import { cache as reactCache } from "react";

const BASE = process.env.OBLETH_ADMIN_BASE_URL ?? "http://localhost:9180";
const AUDIT_ACTOR_HEADER = "X-Obleth-Audit-Actor";

// Resolve the admin token lazily, at request time. Validating it at module
// scope would throw while Next.js evaluates server modules during `next build`
// (and in any environment without the secret), breaking builds and previews.
function adminToken(): string {
  const token = process.env.OBLETH_ADMIN_TOKEN;
  if (!token) {
    throw new Error(
      "OBLETH_ADMIN_TOKEN is not set. The control plane requires the management API admin token to operate.",
    );
  }
  return token;
}

export interface WeeklyWindow {
  day: number; // 0=Sunday .. 6=Saturday
  start_min: number; // minutes from local midnight
  end_min: number;
}

export type GuardrailsAction = "block" | "redact" | "log_only";

export interface GuardrailsPolicy {
  action: GuardrailsAction;
  input_scanners: string[];
  output_scanners: string[];
  guard_model: string | null;
  ban_keywords: string[];
  fail_open: boolean;
}

export interface CompressionPolicy {
  enabled: boolean;
  code_compaction: boolean;
  dedup: boolean;
  compact_logs: boolean;
  allow_lossy: boolean;
}

export interface Tenant {
  id: string;
  name: string;
  fairshare_group: string;
  weight: number;
  tokens_per_minute: number;
  max_in_flight: number | null;
  description: string;
  organization: string;
  contact_email: string;
  status: string;
  timezone: string;
  active_from: string | null;
  active_until: string | null;
  weekly_windows: WeeklyWindow[] | null;
  budget_tokens: number | null;
  budget_cost_usd: number | null;
  budget_period: string | null;
  budget_started_at: string | null;
  allowed_models: string[] | null;
  guardrails_policy: GuardrailsPolicy | null;
  compression_policy: CompressionPolicy | null;
  tracing_enabled: boolean;
  synthetic: boolean;
  created_at: string;
  updated_at: string;
}

export interface ApiKey {
  id: string;
  tenant_id: string;
  name: string;
  description: string;
  key_prefix: string;
  /** `secret` (minted) or `identity` (stands for a verified JWT identity; no secret exists). */
  kind: "secret" | "identity";
  identity_issuer: string | null;
  /** Contract field: external account systems join on it. */
  identity_subject: string | null;
  identity_claims: Record<string, unknown> | null;
  weight: number;
  max_in_flight: number | null;
  budget_tokens: number | null;
  budget_cost_usd: number | null;
  budget_period: string | null;
  budget_started_at: string | null;
  disabled: boolean;
  tracing_enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface CreatedKey {
  key: ApiKey;
  secret: string;
}

export interface ModelRoute {
  id: string;
  model_name: string;
  /**
   * Extra client-facing names that resolve to this same route, so an old
   * spelling keeps working after the canonical name is cleaned up. Only
   * `model_name` is advertised by the gateway's discovery endpoints.
   */
  aliases: string[];
  description: string;
  upstream_model: string;
  api_base: string;
  /** Whether an upstream key is stored. The key itself is write-only and never returned. */
  api_key_set: boolean;
  /**
   * Names of the headers the gateway adds to every upstream request for this
   * model. Values are write-only and never returned. Absent from gateways
   * older than the field.
   */
  upstream_header_names?: string[];
  model_type: string;
  /**
   * Weight/activation format this deployment serves, from the gateway's fixed
   * vocabulary. Descriptive only — it never affects routing. "unknown" means
   * undeclared, which is not the same claim as "none" (full precision).
   */
  quantization: string;
  input_cost_per_token: number;
  output_cost_per_token: number;
  cost_per_image: number;
  cost_per_audio_second: number;
  cost_per_character: number;
  /** Flat USD price of one created job (`video` models). */
  cost_per_video: number;
  energy_slots_per_node: number;
  /** Multiplier on this model's auto-routing score. 1.0 is neutral. */
  route_bias: number;
  /**
   * Whether the auto router may select this model. False removes it from auto's
   * candidate pool while leaving it addressable by name.
   */
  auto_eligible: boolean;
  /** This model's own speculation drafter. Empty = the fleet default. */
  draft_model: string;
  /**
   * Direct URL of a deployment of this model that scores its drafts
   * (prompt_logprobs). Empty = the model cannot speculate.
   */
  verify_api_base: string;
  /** Name that scoring backend serves, if not this model's upstream_model. */
  verify_upstream_model: string;
  context_window: number;
  admission_weight: number;
  max_in_flight: number | null;
  /**
   * `static`, `tuned` or `discovered`. In `discovered` mode the pool size
   * follows the live backend and `max_in_flight` is only the fallback.
   */
  capacity_mode: string;
  capacity_tuned_at: string | null;
  /**
   * Discovered mode: where serving replicas are counted, `endpoints` (the
   * model's enabled, healthy endpoints) or `kubernetes` (Ready pods). The
   * discovery fields are absent from gateways older than the mode.
   */
  capacity_source?: string;
  /** Kubernetes source: namespace of the backend pods; null searches the gateway's list. */
  capacity_namespace?: string | null;
  /** Kubernetes source: label selector for the serving pods; null uses the gateway's template. */
  capacity_selector?: string | null;
  /** Requests one replica takes; null reads it from the source. */
  per_replica_max_in_flight?: number | null;
  /** Multiplier on the derived pool size; 1 is exactly the ready capacity. */
  capacity_headroom?: number;
  supports_function_calling: boolean;
  supports_system_messages: boolean;
  supports_response_schema: boolean;
  supports_tool_choice: boolean;
  supports_vision: boolean;
  enabled: boolean;
  cache_enabled: boolean;
  cache_ttl_secs: number;
  request_timeout_secs: number | null;
  max_retries: number;
  retry_backoff_ms: number;
  endpoint_selection_mode: string;
  debug_diagnostics: boolean;
  tags: string[];
  boons: string[];
  tool_servers: string[];
  created_at: string;
  updated_at: string;
}

/**
 * Model fields accepted by create/update. `api_key` is write-only: omit it to
 * keep the stored key; responses only report `api_key_set`. `upstream_headers`
 * replaces the model's headers when present (a `null` value keeps the stored
 * value for that name); omit it to leave them unchanged. Responses only report
 * `upstream_header_names`.
 */
export type ModelWriteFields = Partial<
  Omit<ModelRoute, "api_key_set" | "upstream_header_names">
> & {
  api_key?: string | null;
  upstream_headers?: Record<string, string | null>;
};

export interface ModelEndpoint {
  id: string;
  model_id: string;
  name: string;
  api_base: string;
  /** Whether an endpoint key is stored. The key itself is write-only and never returned. */
  api_key_set: boolean;
  priority: number;
  weight: number;
  enabled: boolean;
  /**
   * Requests this endpoint takes at once, counted by a discovered model on the
   * `endpoints` source. Null uses the model's per-replica value.
   */
  max_in_flight?: number | null;
  health_status: string;
  consecutive_failures: number;
  alert_state: string;
  last_checked_at: string | null;
  last_latency_ms: number | null;
  last_http_status: number | null;
  last_message: string | null;
  created_at: string;
  updated_at: string;
}

/** The discovered-mode fields sent with a capacity-mode change. */
export interface CapacityDiscoveryFields {
  capacity_source: string;
  capacity_namespace: string | null;
  capacity_selector: string | null;
  per_replica_max_in_flight: number | null;
  capacity_headroom: number;
}

/** What one gateway replica's discovery knows about a discovered model. */
export interface ModelCapacityStatus {
  model_name: string;
  source: string;
  namespaces: string[];
  selector: string | null;
  ready_replicas: number | null;
  per_replica_max_in_flight: number | null;
  /** e.g. `per_replica_max_in_flight`, `endpoint max_in_flight`, `vLLM --max-num-seqs`. */
  per_replica_source: string | null;
  headroom: number;
  /** Last value derived from the source: the cluster-wide pool size. */
  derived_max_in_flight: number | null;
  /** The cluster-wide pool size in force (derived, last kept, or static). */
  effective_max_in_flight: number;
  /** `discovered`, `stale` or `fallback`. */
  state: string;
  last_refresh: string | null;
  last_success: string | null;
  reason: string | null;
}

export interface CapacityDiscoveryModelView {
  model_id: string;
  model_name: string;
  enabled: boolean;
  static_max_in_flight: number | null;
  /** What the answering replica enforces: its share of the effective size. */
  replica_share: number;
  status: ModelCapacityStatus;
}

export interface CapacityDiscoveryView {
  enabled: boolean;
  interval_secs: number;
  namespaces: string[];
  default_selector: string;
  replicas: number;
  models: CapacityDiscoveryModelView[];
}

export interface ManagedModelSpec {
  model_id: string;
  enabled: boolean;
  partition: string;
  gres: string;
  nodes: number;
  constraints: string | null;
  exclude: string | null;
  account: string | null;
  qos: string | null;
  time_limit: string | null;
  cpus_per_task: number | null;
  mem: string | null;
  image: string;
  preamble: string;
  log_output_dir: string;
  launch_command: string;
  script_body: string;
  serving_port: number;
  health_path: string;
  min_replicas: number;
  target_replicas: number;
  max_job_failures: number;
  launcher_spec?: Record<string, unknown> | null;
  last_provision_error?: string | null;
  last_provision_error_at?: string | null;
  created_at: string;
  updated_at: string;
}

export interface PutManagedModel {
  enabled?: boolean;
  partition: string;
  gres?: string;
  nodes?: number;
  constraints?: string | null;
  exclude?: string | null;
  account?: string | null;
  qos?: string | null;
  time_limit?: string | null;
  cpus_per_task?: number | null;
  mem?: string | null;
  image?: string;
  preamble?: string;
  log_output_dir?: string;
  launch_command?: string;
  script_body?: string;
  serving_port: number;
  health_path?: string;
  min_replicas?: number;
  target_replicas?: number;
  max_job_failures?: number;
  launcher_spec?: Record<string, unknown> | null;
}

export type ClusterResources = {
  partitions: {
    name: string;
    nodes: string[];
    default_time: string | null;
    max_time: string | null;
  }[];
  nodes: {
    name: string;
    partitions: string[];
    gres: string;
    cpus: number | null;
    real_memory_mb: number | null;
    features: string[];
  }[];
  accounts: string[];
  qos: string[];
};

export interface ModelReplica {
  id: string;
  model_id: string;
  slurm_job_id: string;
  nodes: string | null;
  endpoint_id: string | null;
  state: string; // pending|starting|healthy|draining|lost
  last_message: string | null;
  cancel_requested?: boolean;
  created_at: string;
  updated_at: string;
}

export type AutotuneKneeReason =
  | "latency_degraded"
  | "plateau"
  | "max_concurrency"
  | "no_data";

export type AutotuneWorkload = "chat" | "coding";

export interface AutotuneStep {
  concurrency: number;
  throughput_rps: number;
  p99_ms: number;
  p50_ms: number;
  requests: number;
  errors: number;
}

export interface AutotuneReport {
  model_id: string;
  model_name: string;
  modality: string;
  workload: AutotuneWorkload;
  recommended_max_in_flight: number;
  knee_reason: AutotuneKneeReason;
  baseline_p99_ms: number;
  latency_ceiling_ms: number;
  latency_headroom: number;
  max_concurrency: number;
  recommended_throughput_rps: number;
  steps: AutotuneStep[];
  duration_ms: number;
}

export interface ModelHealthSummary {
  model_id: string;
  model_name: string;
  checks_enabled: boolean;
  alerts_enabled: boolean;
  check_interval_secs: number;
  failure_threshold: number;
  maintenance_until: string | null;
  maintenance_note: string | null;
  status: string;
  consecutive_failures: number;
  alert_state: string;
  next_check_at: string;
  last_checked_at: string | null;
  last_latency_ms: number | null;
  last_http_status: number | null;
  last_message: string | null;
  updated_at: string;
}

export interface ModelHealthCheck {
  id: number;
  model_id: string;
  checked_at: string;
  trigger: string;
  status: string;
  latency_ms: number | null;
  http_status: number | null;
  message: string | null;
  response_excerpt: string | null;
}

export interface ModelHealthDetail {
  summary: ModelHealthSummary;
  checks: ModelHealthCheck[];
}

export interface BulkModelHealthResult {
  checked: ModelHealthDetail[];
  skipped: number;
}

export interface ValidateModelBody {
  api_base: string;
  api_key?: string | null;
  upstream_model: string;
  model_type?: string | null;
}

export interface ValidateModelResult {
  reachable: boolean;
  wildcard: boolean;
  listed: boolean | null;
  warnings: string[];
}

export interface ModelHealthConfigBody {
  checks_enabled: boolean;
  alerts_enabled: boolean;
  check_interval_secs: number;
  failure_threshold: number;
  maintenance_until?: string | null;
  maintenance_note?: string | null;
}

export interface CacheStats {
  hits: number;
  misses: number;
  tokens_saved: number;
}

export interface McpServer {
  id: string;
  name: string;
  upstream_url: string;
  /** Whether an upstream Authorization header is stored. The value itself is write-only and never returned. */
  auth_header_set: boolean;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface UsageAgg {
  tenant_id: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
}

export interface UsageKeyAgg {
  key_id: string;
  tenant_id: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
}

/// Per-key activity summary: last-used metadata plus rolling usage totals.
/// `last_used_ms` is `0` when the key has no requests in the queried range.
export interface KeyUsageSummary {
  key_id: string;
  tenant_id: string;
  last_used_ms: number;
  last_model: string;
  last_status_code: number;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  cost_usd: number;
  energy_wh: number;
  energy_cost_usd: number;
  co2_g: number;
}

export interface UsageModelAgg {
  model: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  gen_tokens_per_sec: number;
  agg_tokens_per_sec: number;
  avg_ttft_ms: number;
  avg_total_ms: number;
  p50_ttft_ms: number;
  p50_total_ms: number;
  avg_prompt_tokens: number;
  avg_gen_tokens: number;
  users: number;
  energy_wh: number;
  energy_cost_usd: number;
  co2_g: number;
}

export interface UsageTimePoint {
  bucket_ms: number;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
}

/// One row of the permanent daily rollup (`usage_daily`).
export interface UsageDailyRow {
  day: string;
  tenant_id: string;
  key_id: string;
  model: string;
  requests: number;
  success_requests: number;
  error_requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  estimated_tokens: number;
  cache_hits: number;
  cache_misses: number;
  avg_ttft_ms: number;
  avg_total_ms: number;
  /** Total USD spend, summed from each request's frozen completion-time cost. */
  cost_usd: number;
  energy_wh: number;
  energy_cost_usd: number;
  co2_g: number;
}

/// One row of the live request log (`usage/logs`), enriched with tenant/key names.
export interface UsageLogEntry {
  request_id: string;
  ts_ms: number;
  tenant_id: string;
  key_id: string;
  model: string;
  request_type: string;
  session_id: string;
  session_id_source: string;
  device_id: string;
  admission: string;
  status_code: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  queue_wait_ms: number;
  ttft_ms: number;
  total_ms: number;
  cache_status: string;
  cost_usd: number;
  energy_wh: number;
  energy_cost_usd: number;
  co2_g: number;
  tenant_name: string;
  key_name: string;
  key_prefix: string;
  has_trace: boolean;
}

/// One recorded span from the flight-recorder tracer for a single request.
export interface SpanEntry {
  request_id: string;
  span_name: string;
  parent_span: string;
  start_ms: number;
  duration_ms: number;
  status: "ok" | "error";
  attributes: string; // JSON string
}

export type UsageLogStatus = "success" | "error";

export interface UsageLogParams {
  tenantId?: string;
  keyId?: string;
  model?: string;
  requestType?: string;
  sessionId?: string;
  deviceId?: string;
  status?: UsageLogStatus;
  requestId?: string;
  sinceMs?: number;
  untilMs?: number;
  /** Keyset cursor for older pages: rows strictly before this (ts, request id). */
  beforeMs?: number;
  beforeRequestId?: string;
  limit?: number;
  /** When true, only return log entries that have a recorded trace. */
  tracedOnly?: boolean;
  /** When true, include internal traffic (e.g. health probes) hidden by default. */
  includeInternal?: boolean;
}

export interface UsageRetentionView {
  days: number;
  configured: boolean;
}

export interface CompactUsageResult {
  retention_days: number;
  partitions_dropped: number;
}

/** Counts from `POST /api/v1/resync`: entries republished and evicted. */
export interface ResyncReport {
  keys: number;
  keys_pruned: number;
  models: number;
  model_names_pruned: number;
  mcp_servers: number;
  mcp_servers_pruned: number;
}

export type UsageDailyGroupBy =
  | "day"
  | "tenant"
  | "key"
  | "model"
  | "key_model";

export interface UsageDailyParams {
  startDay: string;
  endDay: string;
  groupBy?: UsageDailyGroupBy;
  tenantId?: string;
  keyId?: string;
  model?: string;
}

export interface CostAgg {
  model: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  input_cost: number;
  output_cost: number;
  total_cost: number;
}

export interface LiveStats {
  in_flight: number;
  queued: number;
  /** This replica's share of the enabled models' pool sizes. */
  max_in_flight: number;
  /** Live gateway replicas the configured limits are divided across. */
  replicas?: number;
}

/** Wire shape of GET /overview/summary (config counts + windowed usage totals). */
export interface OverviewSummaryView {
  requests: number;
  tokens: number;
  cost: number;
  has_pricing: boolean;
  tenant_count: number;
  active_tenants: number;
  model_count: number;
  enabled_models: number;
  key_count: number;
}

export interface TenantFairshareView {
  tenant_id: string;
  name: string;
  fairshare_group: string;
  weight: number;
  in_flight: number;
  queued: number;
  served_tokens: number;
  share_score: number;
  weight_share: number;
  expected_slots: number;
  max_in_flight?: number | null;
}

export interface GroupFairshareView {
  name: string;
  weight: number;
  in_flight: number;
  queued: number;
  slot_cap: number;
  served_tokens: number;
  share_score: number;
  weight_share: number;
  expected_slots: number;
}

export interface KeyFairshareView {
  key_id: string;
  tenant_id: string;
  name: string;
  weight: number;
  max_in_flight: number | null;
  in_flight: number;
  queued: number;
  served_tokens: number;
  share_score: number;
  weight_share: number;
  expected_slots: number;
}

export interface ModelPoolView {
  model: string;
  /** Slots this replica enforces: its share of `configured_cap`. */
  cap: number;
  /** Pool size as configured, before it is divided across replicas. */
  configured_cap?: number;
  in_flight: number;
  queued: number;
  borrowed: number;
  groups: GroupFairshareView[];
  tenants: TenantFairshareView[];
  keys: KeyFairshareView[];
}

export interface FairshareLiveView {
  algorithm: string;
  max_in_flight: number;
  global_in_flight: number;
  global_queued: number;
  /** Global in-flight above apportioned caps, borrowed from idle capacity. */
  global_borrowed?: number;
  groups: GroupFairshareView[];
  tenants: TenantFairshareView[];
  /** Live in-flight request count keyed by model name. */
  model_in_flight?: Record<string, number>;
  /** Live queued request count keyed by model name. */
  model_queued?: Record<string, number>;
  /** Hard ceiling on global in-flight admission, independent of pool sums:
   *  this replica's share of the configured ceiling. */
  hard_ceiling?: number;
  /** OBLETH_GLOBAL_MAX_IN_FLIGHT as configured. */
  configured_hard_ceiling?: number;
  /** Enabled models' pool sizes as configured, summed; `max_in_flight` is
   *  this replica's share of it. */
  configured_max_in_flight?: number;
  /** Default per-model in-flight cap applied when a model has none configured. */
  default_model_max_in_flight?: number;
  /** Live gateway replicas the configured limits are divided across. Every
   *  count in the view is the answering replica's own. */
  replicas?: number;
  /** Whether limits are divided across replicas (OBLETH_FAIRSHARE_REPLICA_AWARE). */
  replica_aware?: boolean;
  keys?: KeyFairshareView[];
  pools?: ModelPoolView[];
}

export interface FairshareHistoryPoint {
  ts_ms: number;
  in_flight: number;
  queued: number;
  /** Group name to in-flight slots. */
  groups: Record<string, number>;
}
export interface FairshareHistoryView {
  interval_ms: number;
  retention_ms: number;
  oldest_ts_ms: number | null;
  points: FairshareHistoryPoint[];
}

export interface TenantUsageTimePoint {
  tenant_id: string;
  bucket_ms: number;
  requests: number;
  total_tokens: number;
}

/// Time-bucketed per-model series for the expanded model card charts.
/// Throughput is aggregate tokens/sec over the bucket; latency carries avg + p50.
export interface ModelUsageTimePoint {
  bucket_ms: number;
  requests: number;
  gen_tokens_per_sec: number;
  prompt_tokens_per_sec: number;
  avg_ttft_ms: number;
  p50_ttft_ms: number;
  avg_total_ms: number;
  p50_total_ms: number;
}

/// One tenant/key pair's usage of a single model, with names resolved from
/// Postgres. Powers the breakdown table in the expanded model card.
export interface UsageBreakdownEntry {
  key_id: string;
  tenant_id: string;
  requests: number;
  total_tokens: number;
  gen_tokens_per_sec: number;
  tenant_name: string;
  fairshare_group: string;
  key_name: string;
  key_prefix: string;
}

export interface AuditEntry {
  id: number;
  ts: string;
  actor: string;
  action: string;
  entity_type: string;
  entity_id: string;
  detail: unknown;
}

export interface EmailSettingsView {
  smtp_host: string;
  smtp_port: number;
  username: string | null;
  password_set: boolean;
  from_address: string;
  recipients: string[];
  starttls: boolean;
}

export interface AlertSettingsView {
  slack_webhook_set: boolean;
  min_interval_secs: number;
  email: EmailSettingsView | null;
}

export interface UpdateEmailSettings {
  smtp_host: string;
  smtp_port: number;
  username?: string | null;
  smtp_password?: string | null;
  clear_smtp_password?: boolean;
  from_address: string;
  recipients: string[];
  starttls: boolean;
}

export interface UpdateAlertSettings {
  slack_webhook_url?: string | null;
  clear_slack_webhook?: boolean;
  min_interval_secs?: number;
  email?: UpdateEmailSettings | null;
}

// Masked view of the system-wide Slurm settings. The JWT is never returned;
// presence + last 4 chars are surfaced instead.
export interface SlurmSettingsView {
  enabled: boolean;
  slurmrestd_url: string;
  slurmrestd_api_version: string;
  slurm_user: string;
  jwt_set: boolean;
  jwt_last4: string | null;
  // Operator hostname → IP overrides. When the pods running obleth resolve Slurm
  // node names unreliably, these take DNS out of the loop (endpoints register by
  // IP). Echoed back in full so the form can render and edit them.
  node_aliases: NodeAlias[];
  // Seconds since the provisioner last polled, or null if never seen since the
  // gateway started. provisioner_running is true within the freshness window.
  provisioner_last_seen_secs: number | null;
  provisioner_running: boolean;
  // Build identity the provisioner last reported (it ships as its own image, so
  // it can drift from the gateway version). Null until it has reported.
  provisioner_version: string | null;
  provisioner_git_sha: string | null;
  provisioner_built_at: string | null;
  // Outcome of the provisioner's last reconcile tick ("ok" | "idle" | "error").
  // provisioner_running only proves the process is alive; a provisioner can
  // poll green while every tick fails and replica state sits frozen. Null until
  // reported (or for an older provisioner that doesn't send it).
  provisioner_tick_status: string | null;
  provisioner_tick_detail: string | null;
  // Seconds since the last successful reconcile / length of the current non-ok
  // streak. These — not replica updated_at — are what "states may be stale"
  // keys off.
  provisioner_last_ok_secs: number | null;
  provisioner_held_secs: number | null;
}

export interface UpdateSlurmSettings {
  enabled: boolean;
  slurmrestd_url: string;
  slurmrestd_api_version?: string;
  slurm_user: string;
  // Write-only: omit/empty to keep the stored JWT, send a value to replace it.
  slurm_jwt?: string | null;
  // Full replacement set of node hostname → IP overrides. Blank rows are dropped
  // server-side; a non-blank host must map to a real IP literal.
  node_aliases?: NodeAlias[];
}

// One compute-node hostname → IP override for the Slurm provisioner.
export interface NodeAlias {
  host: string;
  ip: string;
}

export interface SlurmJwtHealth {
  set: boolean;
  expired: boolean;
  expires_at: string | null;
  expires_in_secs: number | null;
}

export interface SlurmPingHealth {
  ok: boolean;
  status_code: number | null;
  latency_ms: number | null;
  error: string | null;
}

export interface SlurmHealthView {
  jwt: SlurmJwtHealth;
  ping: SlurmPingHealth;
}

export interface AutoRouterSettingsView {
  classifier_enabled: boolean;
  classifier_model: string | null;
  classifier_timeout_ms: number;
  available_tags: string[];
  capacity_weight: number;
  cost_weight: number;
  tag_weight: number;
  default_soft_cap: number;
  temperature: number;
  difficulty_enabled: boolean;
  tier_source: "hybrid" | "derived" | "declared";
  messages_default_model: string | null;
}

export interface UpdateAutoRouterSettings {
  classifier_enabled?: boolean;
  classifier_model?: string | null;
  classifier_timeout_ms?: number;
  capacity_weight?: number;
  cost_weight?: number;
  tag_weight?: number;
  default_soft_cap?: number;
  temperature?: number;
  difficulty_enabled?: boolean;
  tier_source?: "hybrid" | "derived" | "declared";
  messages_default_model?: string | null;
}

/// Where a request's routing intent came from. `classifier` never appears in a
/// simulation — the simulate endpoint deliberately does not call the classifier
/// brain, so it reports `heuristic` or `header`.
export type IntentSource = "classifier" | "heuristic" | "header" | "default";

export interface ScoredCandidateView {
  model: string;
  level: number;
  spare: number;
  cost_score: number;
  /**
   * Estimated dollars for this request on this model: unit prices weighted by
   * the prompt estimate and the model's observed average completion length.
   * What `cost_score` normalizes over.
   */
  est_cost: number;
  tag_score: number;
  bias: number;
  score: number;
  chosen: boolean;
}

export interface ReadinessFindingView {
  severity: "warn" | "info";
  code: string;
  title: string;
  detail: string;
  models: string[];
}

/// The routing readiness report: known auto-misroute shapes as findings.
export interface RouterReadinessView {
  findings: ReadinessFindingView[];
  pool_size: number;
  classifier_active: boolean;
  difficulty_enabled: boolean;
}

export interface RejectionView {
  reason: string;
  models: string[];
}

export interface RouteWeightsView {
  capacity: number;
  cost: number;
  tag: number;
  soft_cap: number;
  difficulty_enabled: boolean;
}

/// One `auto` routing decision, explained. The same shape is returned by
/// `simulateRoute` and recorded in the `auto_route` span, so one component
/// renders a hypothetical and a real decision alike.
export interface RouteExplainView {
  chosen: string | null;
  difficulty: number;
  difficulty_source: IntentSource;
  tags: string[];
  tag_source: IntentSource;
  /** Always 0 from a simulation: no classifier ran, so there is no timing. */
  classifier_ms: number;
  tier_domains: string[];
  tier_floor: number;
  tier_floor_clamped: boolean;
  weights: RouteWeightsView;
  temperature: number;
  /**
   * The draw this decision was made with. Ignored at `temperature === 0`.
   * Above it, pin the same value across two simulations that differ only in
   * their weights, or sampling noise reads as an effect of the weight change.
   */
  uniform: number;
  sampled: boolean;
  scored: ScoredCandidateView[];
  rejected: RejectionView[];
}

/// A hypothetical `auto` request. Weight fields are overrides; unset ones fall
/// back to the saved auto-router settings.
export interface SimulateRouteRequest {
  prompt?: string;
  /** Must be an array of chat messages when present; anything else is a 400. */
  messages?: unknown[];
  max_tokens?: number;
  tenant_id?: string;
  effort?: "low" | "medium" | "high";
  needs_function_calling?: boolean;
  needs_tool_choice?: boolean;
  needs_response_schema?: boolean;
  capacity_weight?: number;
  cost_weight?: number;
  tag_weight?: number;
  default_soft_cap?: number;
  temperature?: number;
  difficulty_enabled?: boolean;
  /** Omit to score against the live fleet load, as the gateway itself does. */
  busyness?: Record<string, number>;
  /**
   * Pin the softmax draw in `[0,1)`. Omit for a fresh draw. Pin it when
   * comparing two simulations so only the weight change moves the result.
   */
  uniform?: number;
  /**
   * Derive intent through the LIVE classifier (same brain, cache and timeout
   * the data plane uses) instead of the keyword heuristic. One small model
   * call. Falls back to heuristics when the classifier is off; the response's
   * tag_source says which one ran.
   */
  classify?: boolean;
}

export interface BoonSettingsView {
  vision_enabled: boolean;
  vision_fallback_model: string | null;
  vision_describe_prompt: string;
  vision_max_images: number;
  vision_timeout_ms: number;
  structured_output_enabled: boolean;
  structured_output_fixer_model: string | null;
  structured_output_max_repair_attempts: number;
  structured_output_timeout_ms: number;
  tool_loop_enabled: boolean;
  tool_loop_max_turns: number;
  tool_loop_tool_timeout_ms: number;
  tool_loop_nudge: string;
  compression_enabled: boolean;
  compression_min_tokens: number;
  compression_max_segments: number;
  compression_original_ttl_secs: number;
  compression_max_lossy_segments: number;
  compression_code_compaction: boolean;
  compression_dedup: boolean;
  compression_compact_logs: boolean;
  compression_allow_lossy: boolean;
  compression_neural_keep_ratio: number;
  image_generation_enabled: boolean;
  image_generation_model: string | null;
  image_generation_tool_description: string;
  image_generation_allowed_sizes: string[];
  image_generation_max_images_per_request: number;
  image_generation_timeout_ms: number;
  speculation_enabled: boolean;
  speculation_draft_model: string | null;
  speculation_verify_model: string | null;
  speculation_classify_model: string | null;
  speculation_agree_min: number;
  speculation_lp_min: number;
  speculation_abort_agree: number;
  speculation_abort_lp: number;
  speculation_first_chunk_tokens: number;
  speculation_chunk_tokens: number;
  speculation_decide_by_tokens: number;
  speculation_max_draft_tokens: number;
  speculation_pace_ms: number;
  speculation_timeout_ms: number;
  speculation_draft_chat_template_kwargs: Record<string, unknown> | null;
  speculation_category_gates: SpeculationCategoryGate[];
  speculation_unlisted_categories_speculate: boolean;
  speculation_verify_url_template: string;
}

// One per-category gate of the speculation boon. Missing thresholds fall back
// to the global floors server-side; `speculate: false` excludes the category.
export interface SpeculationCategoryGate {
  tag: string;
  speculate?: boolean;
  agree_min?: number;
  lp_min?: number;
}

export interface UpdateBoonSettings {
  vision_enabled?: boolean;
  vision_fallback_model?: string | null;
  vision_describe_prompt?: string;
  vision_max_images?: number;
  vision_timeout_ms?: number;
  structured_output_enabled?: boolean;
  structured_output_fixer_model?: string | null;
  structured_output_max_repair_attempts?: number;
  structured_output_timeout_ms?: number;
  tool_loop_enabled?: boolean;
  tool_loop_max_turns?: number;
  tool_loop_tool_timeout_ms?: number;
  tool_loop_nudge?: string;
  compression_enabled?: boolean;
  compression_min_tokens?: number;
  compression_max_segments?: number;
  compression_original_ttl_secs?: number;
  compression_max_lossy_segments?: number;
  compression_code_compaction?: boolean;
  compression_dedup?: boolean;
  compression_compact_logs?: boolean;
  compression_allow_lossy?: boolean;
  compression_neural_keep_ratio?: number;
  image_generation_enabled?: boolean;
  image_generation_model?: string | null;
  image_generation_tool_description?: string;
  image_generation_allowed_sizes?: string[];
  image_generation_max_images_per_request?: number;
  image_generation_timeout_ms?: number;
  speculation_enabled?: boolean;
  speculation_draft_model?: string | null;
  speculation_verify_model?: string | null;
  speculation_classify_model?: string | null;
  speculation_agree_min?: number;
  speculation_lp_min?: number;
  speculation_abort_agree?: number;
  speculation_abort_lp?: number;
  speculation_first_chunk_tokens?: number;
  speculation_chunk_tokens?: number;
  speculation_decide_by_tokens?: number;
  speculation_max_draft_tokens?: number;
  speculation_pace_ms?: number;
  speculation_timeout_ms?: number;
  /** An empty object clears the kwargs; omit to leave unchanged. */
  speculation_draft_chat_template_kwargs?: Record<string, unknown>;
  /** Replaces the whole gate list; an empty list clears it. */
  speculation_category_gates?: SpeculationCategoryGate[];
  speculation_unlisted_categories_speculate?: boolean;
  speculation_verify_url_template?: string;
}

// Live status of the optional neural compression sidecar (a health probe of
// OBLETH_COMPRESSOR_URL). Surfaced in the Compression settings tab, mirroring the
// way the Slurm tab shows provisioner health.
export interface CompressorStatusView {
  configured: boolean;
  url: string;
  reachable: boolean;
  model: string | null;
  revision: string | null;
  error: string | null;
}

export interface CharoSettingsView {
  enabled: boolean;
  brain_model: string | null;
  tools_enabled: Record<string, boolean>;
  bench_max_concurrency: number;
  bench_max_duration_s: number;
  bench_max_requests: number;
}

export interface EnergySettingsView {
  enabled: boolean;
  prometheus_url: string;
  power_query: string;
  poll_interval_secs: number;
  energy_cost_per_kwh: number;
  carbon_g_per_kwh: number;
  pue: number;
}

export interface UpdateEnergySettings {
  enabled?: boolean;
  prometheus_url?: string;
  power_query?: string;
  poll_interval_secs?: number;
  energy_cost_per_kwh?: number;
  carbon_g_per_kwh?: number;
  pue?: number;
}

export interface EnergyTestResult {
  cluster_watts: number;
  node_count: number;
}

export interface ChannelResult {
  channel: string;
  ok: boolean;
  detail: string;
}

// ---- config backup / restore ----

export interface BackupEncryptionInfo {
  cipher_enabled: boolean;
  key_check: string | null;
  api_key_pepper_set: boolean;
}

/** Entity arrays in the backup. The dashboard only ever counts them. */
export interface ConfigBackupData {
  fairshare_groups: unknown[];
  tenants: unknown[];
  api_keys: unknown[];
  models: unknown[];
  model_endpoints: unknown[];
  mcp_servers: unknown[];
  app_settings: unknown[];
}

export interface ConfigBackup {
  format: string;
  version: number;
  exported_at: string;
  gateway_version: string;
  encryption: BackupEncryptionInfo;
  data: ConfigBackupData;
}

/// The `obleth-models` manifest: model configuration as a portable, editable
/// file. Distinct from ConfigBackup, which is a whole-instance snapshot keyed
/// by uuid. Every field but model_name is optional — absent means "leave
/// unchanged" — so this is deliberately loosely typed on the way in.
export interface ModelManifest {
  format: string;
  version: number;
  exported_at?: string;
  gateway_version?: string;
  models: ManifestModel[];
}

export interface ManifestModel {
  model_name: string;
  tags?: string[];
  boons?: string[];
  enabled?: boolean;
  /** Export-only presence flag; the key itself is never exported. */
  has_api_key?: boolean;
  endpoints?: unknown[];
  [key: string]: unknown;
}

export interface ModelImportEntry {
  model_name: string;
  /** "created" | "updated" | "unchanged" */
  action: string;
  changed_fields: string[];
  warnings: string[];
}

export interface ModelImportReport {
  dry_run: boolean;
  created: number;
  updated: number;
  unchanged: number;
  models: ModelImportEntry[];
}

export interface RestoreCounts {
  inserted: number;
  updated: number;
}

export interface RestoreReport {
  fairshare_groups: RestoreCounts;
  tenants: RestoreCounts;
  api_keys: RestoreCounts;
  models: RestoreCounts;
  model_endpoints: RestoreCounts;
  mcp_servers: RestoreCounts;
  app_settings: RestoreCounts;
  warnings: string[];
}

export interface TestAlertResult {
  results: ChannelResult[];
}

export interface Recipe {
  id: string;
  name: string;
  body: string;
  author: string;
}

// Mirrors `CollectionView` in obleth-admin/src/knowledge/mod.rs.
export interface KnowledgeCollection {
  id: string;
  name: string;
  description: string;
  embedding_model: string;
  indexed_embedding_model: string;
  embedding_dim: number;
  chunk_tokens: number;
  chunk_overlap_tokens: number;
  needs_reindex: boolean;
  chunk_count: number;
  estimated_bytes: number;
}

// Mirrors `DocumentView` in obleth-admin/src/knowledge/mod.rs.
export interface KnowledgeDocument {
  id: string;
  collection_id: string;
  title: string;
  filename: string;
  content_type: string;
  byte_size: number;
  status: "pending" | "indexing" | "ready" | "failed";
  error: string | null;
  chunk_count: number;
  // True only alongside status "indexing": a reindex was requested while
  // this document was already indexing, and this document's own in-flight
  // run will requeue it once that run finishes, rather than a second worker
  // claiming it immediately.
  reindex_requested: boolean;
}

// Mirrors `SearchHit` in obleth-admin/src/knowledge/mod.rs.
export interface KnowledgeHit {
  chunk_id: string;
  document_id: string;
  score: number;
  token_count: number;
  text: string;
  would_inject: boolean;
}

// Mirrors `ReindexResult` in obleth-admin/src/knowledge/mod.rs.
export interface KnowledgeReindexResult {
  documents_requeued: number;
}

// Mirrors `ModelCollectionsView` in obleth-admin/src/knowledge/mod.rs.
export interface ModelKnowledgeCollections {
  collection_ids: string[];
}

// Mirrors `KnowledgeSettingsView` in obleth-admin/src/knowledge/mod.rs.
export interface KnowledgeSettingsView {
  enabled: boolean;
  top_k: number;
  min_score: number;
  max_context_tokens: number;
  embed_timeout_ms: number;
  query_cache_ttl_s: number;
  query_turns: number;
  max_upload_bytes: number;
  max_chunks_per_collection: number;
  index_batch_size: number;
  index_timeout_ms: number;
  index_stale_after_secs: number;
  debug_snapshot: boolean;
}

// Mirrors `UpdateKnowledgeSettings` in obleth-admin/src/knowledge/mod.rs.
export interface UpdateKnowledgeSettings {
  enabled?: boolean;
  top_k?: number;
  min_score?: number;
  max_context_tokens?: number;
  embed_timeout_ms?: number;
  query_cache_ttl_s?: number;
  query_turns?: number;
  max_upload_bytes?: number;
  max_chunks_per_collection?: number;
  index_batch_size?: number;
  index_timeout_ms?: number;
  index_stale_after_secs?: number;
  debug_snapshot?: boolean;
}

/// Error thrown when the management API responds with a non-2xx status. Carries
/// the parsed `error` message so the UI can display something actionable.
export class OblethApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
    public readonly path: string,
  ) {
    super(message);
    this.name = "OblethApiError";
  }
}

/** Next.js fetch caching options accepted alongside a standard RequestInit. */
type NextFetchOptions = { revalidate?: number | false; tags?: string[] };
type ApiInit = RequestInit & { next?: NextFetchOptions };
interface AuditOptions {
  auditActor?: string | null;
}

function auditActorHeaders(options?: AuditOptions): Record<string, string> {
  const rawActor = options?.auditActor?.trim();
  const actor = rawActor?.replace(/[\r\n]+/g, " ");
  return actor ? { [AUDIT_ACTOR_HEADER]: actor } : {};
}

async function api<T>(path: string, init?: ApiInit): Promise<T> {
  const { next, cache, headers, ...rest } = init ?? {};
  const fetchInit: ApiInit = {
    ...rest,
    headers: {
      Authorization: `Bearer ${adminToken()}`,
      "Content-Type": "application/json",
      ...(headers ?? {}),
    },
  };
  // Default to `no-store` so reads are always fresh. Callers may opt specific
  // GETs into Next's Data Cache by passing `next: { revalidate }`; those routes
  // already call `revalidatePath` on mutation, so cached lists stay correct.
  if (next) fetchInit.next = next;
  else fetchInit.cache = cache ?? "no-store";

  const res = await fetch(`${BASE}/api/v1${path}`, fetchInit);
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    // The management API returns errors as `{"error": "..."}`. Surface that
    // message directly so callers (and the UI) get something actionable instead
    // of a raw status dump.
    let message = text;
    try {
      const parsed = JSON.parse(text);
      if (parsed && typeof parsed.error === "string") message = parsed.error;
    } catch {
      // Non-JSON body; fall back to the raw text.
    }
    throw new OblethApiError(
      res.status,
      message || `request failed with status ${res.status}`,
      path,
    );
  }
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

function qs(params: Record<string, string | number | undefined>) {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined) q.set(k, String(v));
  }
  const s = q.toString();
  return s ? `?${s}` : "";
}

/** Build/version identity reported by the gateway's public /version endpoint. */
export interface VersionInfo {
  version: string;
  git_sha: string | null;
  built_at: string | null;
}

// The slow-changing entity lists are opted into Next's Data Cache with a short
// revalidate window plus a tag. Every mutating server action calls
// `revalidateTag` for the lists it changes, so dashboards see writes
// immediately while auto-refresh polling stops re-fetching unchanged lists
// from the admin API on every tick. The window only bounds staleness for
// writes made outside the control plane (e.g. direct admin-API calls).
const LIST_REVALIDATE_SECS = 30;
export const CACHE_TAGS = {
  tenants: "tenants",
  keys: "keys",
  models: "models",
  knowledgeCollections: "knowledge-collections",
} as const;

export const obleth = {
  gatewayVersion: () =>
    api<VersionInfo>("/version", { next: { revalidate: 300 } }),
  listTenants: () =>
    api<Tenant[]>("/tenants", {
      next: { revalidate: LIST_REVALIDATE_SECS, tags: [CACHE_TAGS.tenants] },
    }),
  createTenant: (
    body: {
      name: string;
      weight?: number;
      tokens_per_minute?: number;
      max_in_flight?: number | null;
      fairshare_group?: string;
      synthetic?: boolean;
    },
    options?: AuditOptions,
  ) =>
    api<Tenant>("/tenants", {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  setWeight: (id: string, weight: number, options?: AuditOptions) =>
    api<Tenant>(`/tenants/${id}/weight`, {
      method: "PATCH",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ weight }),
    }),
  setQuota: (
    id: string,
    tokens_per_minute: number,
    max_in_flight: number | null,
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${id}/quota`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ tokens_per_minute, max_in_flight }),
    }),
  updateTenant: (
    id: string,
    body: {
      name: string;
      description?: string;
      organization?: string;
      contact_email?: string;
    },
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${id}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  setTenantStatus: (id: string, status: string, options?: AuditOptions) =>
    api<Tenant>(`/tenants/${id}/status`, {
      method: "PATCH",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ status }),
    }),
  setTenantSchedule: (
    id: string,
    body: {
      timezone: string;
      active_from?: string | null;
      active_until?: string | null;
      weekly_windows?: WeeklyWindow[] | null;
    },
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${id}/schedule`, {
      method: "PATCH",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  setTenantBudget: (
    id: string,
    body: {
      budget_tokens?: number | null;
      budget_cost_usd?: number | null;
      budget_period?: string | null;
      budget_started_at?: string | null;
    },
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${id}/budget`, {
      method: "PATCH",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  setTenantAllowlist: (
    id: string,
    allowed_models: string[],
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${id}/allowlist`, {
      method: "PATCH",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ allowed_models }),
    }),
  setTenantGuardrails: (
    id: string,
    policy: GuardrailsPolicy | null,
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${id}/guardrails`, {
      method: "PATCH",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ policy }),
    }),
  setTenantCompression: (
    id: string,
    policy: CompressionPolicy | null,
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${id}/compression`, {
      method: "PATCH",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ policy }),
    }),
  deleteTenant: (id: string, options?: AuditOptions) =>
    api<void>(`/tenants/${id}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  listKeys: (tenantId?: string) =>
    api<ApiKey[]>(`/keys${tenantId ? `?tenant_id=${tenantId}` : ""}`, {
      next: { revalidate: LIST_REVALIDATE_SECS, tags: [CACHE_TAGS.keys] },
    }),
  createKey: (
    tenantId: string,
    body: {
      name: string;
      description?: string;
      weight?: number;
      max_in_flight?: number | null;
      budget_tokens?: number | null;
      budget_cost_usd?: number | null;
      budget_period?: string | null;
      budget_started_at?: string | null;
    },
    options?: AuditOptions,
  ) =>
    api<CreatedKey>(`/tenants/${tenantId}/keys`, {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  updateKey: (
    id: string,
    body: {
      name: string;
      description?: string;
      weight?: number;
      max_in_flight?: number | null;
      budget_tokens?: number | null;
      budget_cost_usd?: number | null;
      budget_period?: string | null;
      budget_started_at?: string | null;
    },
    options?: AuditOptions,
  ) =>
    api<ApiKey>(`/keys/${id}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  setKeyDisabled: (id: string, disabled: boolean, options?: AuditOptions) =>
    api<void>(`/keys/${id}/disabled`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ disabled }),
    }),
  setKeyTracing: (id: string, tracing_enabled: boolean, options?: AuditOptions) =>
    api<void>(`/keys/${id}/tracing`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ tracing_enabled }),
    }),
  setTenantTracing: (
    id: string,
    tracing_enabled: boolean,
    options?: AuditOptions,
  ) =>
    api<void>(`/tenants/${id}/tracing`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ tracing_enabled }),
    }),
  setTenantSynthetic: (
    id: string,
    synthetic: boolean,
    options?: AuditOptions,
  ) =>
    api<void>(`/tenants/${id}/synthetic`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ synthetic }),
    }),
  deleteKey: (id: string, options?: AuditOptions) =>
    api<void>(`/keys/${id}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  listModels: () =>
    api<ModelRoute[]>("/models", {
      next: { revalidate: LIST_REVALIDATE_SECS, tags: [CACHE_TAGS.models] },
    }),
  createModel: (
    body: ModelWriteFields & {
      model_name: string;
      upstream_model: string;
      api_base: string;
    },
    options?: AuditOptions,
  ) =>
    api<ModelRoute>("/models", {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  updateModel: (
    id: string,
    body: ModelWriteFields & { upstream_model: string; api_base: string },
    options?: AuditOptions,
  ) =>
    api<ModelRoute>(`/models/${id}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteModel: (id: string, options?: AuditOptions) =>
    api<void>(`/models/${id}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  validateModel: (body: ValidateModelBody) =>
    api<ValidateModelResult>("/models/validate", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  modelHealth: () => api<ModelHealthSummary[]>("/models/health"),
  modelHealthDetail: (id: string) =>
    api<ModelHealthDetail>(`/models/${id}/health`),
  checkModelHealth: (id: string) =>
    api<ModelHealthDetail>(`/models/${id}/health/check`, { method: "POST" }),
  checkAllModelHealth: () =>
    api<BulkModelHealthResult>("/models/health/check", { method: "POST" }),
  setModelHealthConfig: (
    id: string,
    body: ModelHealthConfigBody,
    options?: AuditOptions,
  ) =>
    api<ModelHealthSummary>(`/models/${id}/health/config`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  setModelWeight: (
    id: string,
    admission_weight: number,
    options?: AuditOptions,
  ) =>
    api<ModelRoute>(`/models/${id}/weight`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ admission_weight }),
    }),
  setModelCapacity: (
    id: string,
    max_in_flight: number | null,
    options?: AuditOptions,
  ) =>
    api<ModelRoute>(`/models/${id}/capacity`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ max_in_flight }),
    }),
  setModelCapacityMode: (
    id: string,
    capacity_mode: string,
    fields?: CapacityDiscoveryFields,
    options?: AuditOptions,
  ) =>
    api<ModelRoute>(`/models/${id}/capacity-mode`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ capacity_mode, ...(fields ?? {}) }),
    }),
  capacityDiscovery: () => api<CapacityDiscoveryView>("/capacity/discovery"),
  autotuneModel: (
    id: string,
    opts?: {
      workload?: AutotuneWorkload;
      latency_headroom?: number;
      replicas?: number;
    },
    options?: AuditOptions,
  ) =>
    api<AutotuneReport>(`/models/${id}/autotune`, {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify(opts ?? {}),
    }),
  applyAutotuneCapacity: (
    id: string,
    max_in_flight: number,
    options?: AuditOptions,
  ) =>
    api<ModelRoute>(`/models/${id}/autotune/apply`, {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ max_in_flight }),
    }),
  setModelCache: (
    id: string,
    cache_enabled: boolean,
    cache_ttl_secs?: number,
    options?: AuditOptions,
  ) =>
    api<ModelRoute>(`/models/${id}/cache`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ cache_enabled, cache_ttl_secs }),
    }),
  setModelReliability: (
    id: string,
    body: {
      request_timeout_secs: number | null;
      max_retries: number;
      retry_backoff_ms: number;
      endpoint_selection_mode: string;
      debug_diagnostics: boolean;
    },
    options?: AuditOptions,
  ) =>
    api<ModelRoute>(`/models/${id}/reliability`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  listModelEndpoints: (id: string) =>
    api<ModelEndpoint[]>(`/models/${id}/endpoints`),
  getManagedModel: (id: string) =>
    api<ManagedModelSpec | null>(`/models/${id}/managed`),
  listManagedModels: () => api<ManagedModelSpec[]>("/managed"),
  putManagedModel: (
    id: string,
    body: PutManagedModel,
    options?: AuditOptions,
  ) =>
    api<ManagedModelSpec>(`/models/${id}/managed`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteManagedModel: (id: string, options?: AuditOptions) =>
    api<void>(`/models/${id}/managed`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  clearProvisionError: (id: string, options?: AuditOptions) =>
    api<{ ok: boolean }>(`/models/${id}/managed/provision-error`, {
      method: "PATCH",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ error: null }),
    }),
  slurmResources: () => api<ClusterResources>(`/slurm/resources`),
  listRecipes: () => api<Recipe[]>(`/recipes`),
  createRecipe: (
    body: { name: string; body: string; author?: string },
    options?: AuditOptions,
  ) =>
    api<Recipe>(`/recipes`, {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  updateRecipe: (
    id: string,
    body: { name: string; body: string; author?: string },
    options?: AuditOptions,
  ) =>
    api<Recipe>(`/recipes/${id}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteRecipe: (id: string, options?: AuditOptions) =>
    api<void>(`/recipes/${id}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  listReplicas: (id: string) =>
    api<ModelReplica[]>(`/models/${id}/replicas`),
  clearLostReplicas: (id: string, options?: AuditOptions) =>
    api<{ deleted: number }>(`/models/${id}/replicas/clear-lost`, {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  restartReplica: (replicaId: string, options?: AuditOptions) =>
    api<{ ok: boolean }>(`/replicas/${replicaId}/restart`, {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  createModelEndpoint: (
    id: string,
    body: {
      name: string;
      api_base: string;
      api_key?: string | null;
      priority?: number;
      weight?: number;
      enabled?: boolean;
      max_in_flight?: number | null;
    },
    options?: AuditOptions,
  ) =>
    api<ModelEndpoint>(`/models/${id}/endpoints`, {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  updateModelEndpoint: (
    id: string,
    endpointId: string,
    body: {
      name: string;
      api_base: string;
      api_key?: string | null;
      priority?: number;
      weight?: number;
      enabled?: boolean;
      max_in_flight?: number | null;
    },
    options?: AuditOptions,
  ) =>
    api<ModelEndpoint>(`/models/${id}/endpoints/${endpointId}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteModelEndpoint: (id: string, endpointId: string, options?: AuditOptions) =>
    api<void>(`/models/${id}/endpoints/${endpointId}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  cacheStats: (sinceMs?: number) =>
    api<CacheStats>(`/usage/cache${qs({ since_ms: sinceMs })}`),
  listMcpServers: () => api<McpServer[]>("/mcp-servers"),
  createMcpServer: (
    body: {
      name: string;
      upstream_url: string;
      auth_header?: string;
    },
    options?: AuditOptions,
  ) =>
    api<McpServer>("/mcp-servers", {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  updateMcpServer: (
    id: string,
    body: { upstream_url: string; auth_header?: string; enabled?: boolean },
    options?: AuditOptions,
  ) =>
    api<McpServer>(`/mcp-servers/${id}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteMcpServer: (id: string, options?: AuditOptions) =>
    api<void>(`/mcp-servers/${id}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  usage: (sinceMs?: number) =>
    api<UsageAgg[]>(`/usage${qs({ since_ms: sinceMs })}`),
  usageByKey: (sinceMs?: number, limit?: number) =>
    api<UsageKeyAgg[]>(`/usage/keys${qs({ since_ms: sinceMs, limit })}`),
  keyUsage: (id: string, sinceMs?: number) =>
    api<KeyUsageSummary>(`/keys/${id}/usage${qs({ since_ms: sinceMs })}`),
  usageKeysSummary: (
    params: { tenantId?: string; sinceMs?: number; limit?: number } = {},
  ) =>
    api<KeyUsageSummary[]>(
      `/usage/keys/summary${qs({
        tenant_id: params.tenantId,
        since_ms: params.sinceMs,
        limit: params.limit,
      })}`,
    ),
  /** Bulk per-key summary with automatic fallback to `/usage/keys` while summary is rolling out. */
  keyUsageForDashboard: async (
    params: { sinceMs?: number; limit?: number } = {},
  ) => {
    try {
      return await api<KeyUsageSummary[]>(
        `/usage/keys/summary${qs({ since_ms: params.sinceMs, limit: params.limit })}`,
      );
    } catch {
      const legacy = await api<UsageKeyAgg[]>(
        `/usage/keys${qs({ since_ms: params.sinceMs, limit: params.limit })}`,
      ).catch(() => [] as UsageKeyAgg[]);
      return legacy.map((u) => ({
        key_id: u.key_id,
        tenant_id: u.tenant_id,
        last_used_ms: 0,
        last_model: "",
        last_status_code: 0,
        requests: u.requests,
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        total_tokens: u.total_tokens,
        cost_usd: 0,
        energy_wh: 0,
        energy_cost_usd: 0,
        co2_g: 0,
      }));
    }
  },
  usageByModel: (sinceMs?: number) =>
    api<UsageModelAgg[]>(`/usage/models${qs({ since_ms: sinceMs })}`),
  usageSeries: (bucketMs = 300_000, sinceMs?: number) =>
    api<UsageTimePoint[]>(
      `/usage/series${qs({ bucket_ms: bucketMs, since_ms: sinceMs })}`,
    ),
  usageSeriesByTenant: (bucketMs = 10_000, sinceMs?: number) =>
    api<TenantUsageTimePoint[]>(
      `/usage/series/tenants${qs({ bucket_ms: bucketMs, since_ms: sinceMs })}`,
    ),
  usageSeriesByModel: (model: string, bucketMs = 60_000, sinceMs?: number) =>
    api<ModelUsageTimePoint[]>(
      `/usage/series/models${qs({
        model,
        bucket_ms: bucketMs,
        since_ms: sinceMs,
      })}`,
    ),
  usageBreakdownByModel: (model: string, sinceMs?: number, limit?: number) =>
    api<UsageBreakdownEntry[]>(
      `/usage/breakdown${qs({ model, since_ms: sinceMs, limit })}`,
    ),
  costs: (sinceMs?: number) =>
    api<CostAgg[]>(`/costs${qs({ since_ms: sinceMs })}`),
  usageDaily: (params: UsageDailyParams) =>
    api<UsageDailyRow[]>(
      `/usage/daily${qs({
        start_day: params.startDay,
        end_day: params.endDay,
        group_by: params.groupBy,
        tenant_id: params.tenantId,
        key_id: params.keyId,
        model: params.model,
      })}`,
    ),
  usageLogs: (params: UsageLogParams = {}) =>
    api<UsageLogEntry[]>(
      `/usage/logs${qs({
        tenant_id: params.tenantId,
        key_id: params.keyId,
        model: params.model,
        request_type: params.requestType,
        session_id: params.sessionId,
        device_id: params.deviceId,
        status: params.status,
        request_id: params.requestId,
        since_ms: params.sinceMs,
        until_ms: params.untilMs,
        before_ms: params.beforeMs,
        before_request_id: params.beforeRequestId,
        limit: params.limit,
        traced_only: params.tracedOnly ? "true" : undefined,
        include_internal: params.includeInternal ? "true" : undefined,
      })}`,
    ),
  getRequestSpans: (requestId: string) =>
    api<SpanEntry[]>(`/usage/logs/${requestId}/spans`).catch(() => [] as SpanEntry[]),
  // Server-only: hand the control-plane (Charo's model-test console) the reserved
  // system key secret so it can call the data plane as the protected internal tenant.
  controlPlaneKey: () =>
    api<{ secret: string }>("/system/control-plane-key"),
  getUsageRetention: reactCache(() => api<UsageRetentionView>("/settings/usage-retention")),
  setUsageRetention: (days: number, options?: AuditOptions) =>
    api<UsageRetentionView>("/settings/usage-retention", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ days }),
    }),
  compactUsage: (options?: AuditOptions) =>
    api<CompactUsageResult>("/usage/compact", {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  resync: (options?: AuditOptions) =>
    api<ResyncReport>("/resync", {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  stats: () => api<LiveStats>("/stats"),
  overviewSummary: (sinceMs?: number) =>
    api<OverviewSummaryView>(`/overview/summary${qs({ since_ms: sinceMs })}`),
  fairshareLive: () => api<FairshareLiveView>("/fairshare/live"),
  fairshareHistory: (params: { since_ms?: number; model?: string } = {}) =>
    api<FairshareHistoryView>(
      `/fairshare/history${qs({ since_ms: params.since_ms, model: params.model })}`,
    ),
  audit: (limit = 100) => api<AuditEntry[]>(`/audit?limit=${limit}`),
  getCapacity: () => api<{ max_in_flight: number }>("/capacity"),
  setCapacity: (max_in_flight: number, options?: AuditOptions) =>
    api<{ max_in_flight: number }>("/capacity", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ max_in_flight }),
    }),
  getAlertSettings: reactCache(() => api<AlertSettingsView>("/settings/alerts")),
  setAlertSettings: (body: UpdateAlertSettings, options?: AuditOptions) =>
    api<AlertSettingsView>("/settings/alerts", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  testAlert: () =>
    api<TestAlertResult>("/settings/alerts/test", { method: "POST" }),
  getAutoRouterSettings: reactCache(() =>
    api<AutoRouterSettingsView>("/settings/auto-router"),
  ),
  setAutoRouterSettings: (
    body: UpdateAutoRouterSettings,
    options?: AuditOptions,
  ) =>
    api<AutoRouterSettingsView>("/settings/auto-router", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  /// Run the whole `auto` pipeline against the live fleet and return the
  /// decision it would make, without dispatching anything. Read-only: no audit
  /// entry and no upstream dispatch; `classify: true` opts into one small
  /// classifier call so the simulated tags match what serving would derive.
  simulateRoute: (body: SimulateRouteRequest) =>
    api<RouteExplainView>("/router/simulate", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  /// Routing readiness lints: the known auto-misroute shapes, as findings.
  getRouterReadiness: reactCache(() =>
    api<RouterReadinessView>("/router/readiness"),
  ),
  getBoonSettings: reactCache(() => api<BoonSettingsView>("/settings/boons")),
  setBoonSettings: (body: UpdateBoonSettings, options?: AuditOptions) =>
    api<BoonSettingsView>("/settings/boons", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  getCompressorStatus: reactCache(() => api<CompressorStatusView>("/settings/compressor")),
  getCharoSettings: reactCache(() => api<CharoSettingsView>("/settings/charo")),
  setCharoSettings: (body: CharoSettingsView, options?: AuditOptions) =>
    api<CharoSettingsView>("/settings/charo", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  getEnergySettings: reactCache(() => api<EnergySettingsView>("/settings/energy")),
  setEnergySettings: (body: UpdateEnergySettings, options?: AuditOptions) =>
    api<EnergySettingsView>("/settings/energy", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  testEnergyQuery: (prometheus_url: string, power_query: string) =>
    api<EnergyTestResult>("/settings/energy/test", {
      method: "POST",
      body: JSON.stringify({ prometheus_url, power_query }),
    }),
  getSlurmSettings: reactCache(() => api<SlurmSettingsView>("/settings/slurm")),
  setSlurmSettings: (body: UpdateSlurmSettings, options?: AuditOptions) =>
    api<SlurmSettingsView>("/settings/slurm", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  testSlurmConnection: () =>
    api<SlurmHealthView>("/settings/slurm/test", { method: "POST" }),
  exportBackup: (options?: AuditOptions) =>
    api<ConfigBackup>("/backup/export", {
      headers: auditActorHeaders(options),
    }),
  restoreBackup: (body: ConfigBackup, options?: AuditOptions) =>
    api<RestoreReport>("/backup/restore", {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  exportModels: (options?: AuditOptions) =>
    api<ModelManifest>("/models/export", {
      headers: auditActorHeaders(options),
    }),
  importModels: (
    body: ModelManifest,
    opts: { dryRun: boolean } & AuditOptions,
  ) =>
    api<ModelImportReport>(
      `/models/import?dry_run=${opts.dryRun ? "true" : "false"}`,
      {
        method: "POST",
        headers: auditActorHeaders(opts),
        body: JSON.stringify(body),
      },
    ),
  listCollections: () =>
    api<KnowledgeCollection[]>("/knowledge/collections", {
      next: {
        revalidate: LIST_REVALIDATE_SECS,
        tags: [CACHE_TAGS.knowledgeCollections],
      },
    }),
  createCollection: (
    body: { name: string; description?: string; embedding_model: string },
    options?: AuditOptions,
  ) =>
    api<KnowledgeCollection>("/knowledge/collections", {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  getCollection: (id: string) =>
    api<KnowledgeCollection>(`/knowledge/collections/${id}`),
  updateCollection: (
    id: string,
    body: {
      name?: string;
      description?: string;
      embedding_model?: string;
      chunk_tokens?: number;
      chunk_overlap_tokens?: number;
    },
    options?: AuditOptions,
  ) =>
    api<KnowledgeCollection>(`/knowledge/collections/${id}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteCollection: (id: string, options?: AuditOptions) =>
    api<void>(`/knowledge/collections/${id}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  listDocuments: (collectionId: string) =>
    api<KnowledgeDocument[]>(
      `/knowledge/collections/${collectionId}/documents`,
    ),
  uploadDocument: (
    collectionId: string,
    body: { title: string; filename: string; content_base64: string },
    options?: AuditOptions,
  ) =>
    api<KnowledgeDocument>(
      `/knowledge/collections/${collectionId}/documents`,
      {
        method: "POST",
        headers: auditActorHeaders(options),
        body: JSON.stringify(body),
      },
    ),
  deleteDocument: (id: string, options?: AuditOptions) =>
    api<void>(`/knowledge/documents/${id}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  reindexCollection: (id: string, options?: AuditOptions) =>
    api<KnowledgeReindexResult>(`/knowledge/collections/${id}/reindex`, {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  reindexDocument: (id: string, options?: AuditOptions) =>
    api<KnowledgeDocument>(`/knowledge/documents/${id}/reindex`, {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  searchCollection: (id: string, body: { query: string; top_k?: number }) =>
    api<KnowledgeHit[]>(`/knowledge/collections/${id}/search`, {
      method: "POST",
      body: JSON.stringify(body),
    }),
  getModelCollections: (modelId: string) =>
    api<ModelKnowledgeCollections>(`/models/${modelId}/knowledge`),
  setModelCollections: (
    modelId: string,
    collection_ids: string[],
    options?: AuditOptions,
  ) =>
    api<ModelKnowledgeCollections>(`/models/${modelId}/knowledge`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ collection_ids }),
    }),
  getKnowledgeSettings: reactCache(() =>
    api<KnowledgeSettingsView>("/settings/knowledge"),
  ),
  updateKnowledgeSettings: (
    body: UpdateKnowledgeSettings,
    options?: AuditOptions,
  ) =>
    api<KnowledgeSettingsView>("/settings/knowledge", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
};
