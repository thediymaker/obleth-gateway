// Server-side client for the obleth Management API.

const BASE = process.env.OBLETH_ADMIN_BASE_URL ?? "http://localhost:9180";

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
  tracing_enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface ApiKey {
  id: string;
  tenant_id: string;
  name: string;
  description: string;
  key_prefix: string;
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
  description: string;
  upstream_model: string;
  api_base: string;
  api_key: string | null;
  model_type: string;
  input_cost_per_token: number;
  output_cost_per_token: number;
  cost_per_image: number;
  cost_per_audio_second: number;
  cost_per_character: number;
  context_window: number;
  admission_weight: number;
  max_in_flight: number | null;
  capacity_mode: string;
  capacity_tuned_at: string | null;
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
  tags: string[];
  boons: string[];
  tool_servers: string[];
  created_at: string;
  updated_at: string;
}

export interface ModelEndpoint {
  id: string;
  model_id: string;
  name: string;
  api_base: string;
  api_key: string | null;
  priority: number;
  weight: number;
  enabled: boolean;
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
  target_replicas: number;
  max_job_failures: number;
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
  target_replicas?: number;
  max_job_failures?: number;
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
  auth_header: string | null;
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
}

export interface UsageRetentionView {
  days: number;
  configured: boolean;
}

export interface CompactUsageResult {
  retention_days: number;
  partitions_dropped: number;
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
  max_in_flight: number;
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

export interface FairshareLiveView {
  algorithm: string;
  max_in_flight: number;
  global_in_flight: number;
  global_queued: number;
  groups: GroupFairshareView[];
  tenants: TenantFairshareView[];
  /** Live in-flight request count keyed by model name. */
  model_in_flight?: Record<string, number>;
  /** Live queued request count keyed by model name. */
  model_queued?: Record<string, number>;
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
}

export interface UpdateSlurmSettings {
  enabled: boolean;
  slurmrestd_url: string;
  slurmrestd_api_version?: string;
  slurm_user: string;
  // Write-only: omit/empty to keep the stored JWT, send a value to replace it.
  slurm_jwt?: string | null;
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
}

export interface UpdateAutoRouterSettings {
  classifier_enabled?: boolean;
  classifier_model?: string | null;
  classifier_timeout_ms?: number;
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
}

export interface CharoSettingsView {
  enabled: boolean;
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

export type SavedRecipe = {
  id: string;
  name: string;
  backend: string;
  author: string;
  spec: Record<string, unknown>;
};

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
} as const;

export const obleth = {
  gatewayVersion: () =>
    api<VersionInfo>("/version", { next: { revalidate: 300 } }),
  listTenants: () =>
    api<Tenant[]>("/tenants", {
      next: { revalidate: LIST_REVALIDATE_SECS, tags: [CACHE_TAGS.tenants] },
    }),
  createTenant: (body: {
    name: string;
    weight?: number;
    tokens_per_minute?: number;
    max_in_flight?: number | null;
    fairshare_group?: string;
  }) => api<Tenant>("/tenants", { method: "POST", body: JSON.stringify(body) }),
  setWeight: (id: string, weight: number) =>
    api<Tenant>(`/tenants/${id}/weight`, {
      method: "PATCH",
      body: JSON.stringify({ weight }),
    }),
  setQuota: (
    id: string,
    tokens_per_minute: number,
    max_in_flight: number | null,
  ) =>
    api<Tenant>(`/tenants/${id}/quota`, {
      method: "PUT",
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
  ) =>
    api<Tenant>(`/tenants/${id}`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  setTenantStatus: (id: string, status: string) =>
    api<Tenant>(`/tenants/${id}/status`, {
      method: "PATCH",
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
  ) =>
    api<Tenant>(`/tenants/${id}/schedule`, {
      method: "PATCH",
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
  ) =>
    api<Tenant>(`/tenants/${id}/budget`, {
      method: "PATCH",
      body: JSON.stringify(body),
    }),
  setTenantAllowlist: (id: string, allowed_models: string[]) =>
    api<Tenant>(`/tenants/${id}/allowlist`, {
      method: "PATCH",
      body: JSON.stringify({ allowed_models }),
    }),
  setTenantGuardrails: (id: string, policy: GuardrailsPolicy | null) =>
    api<Tenant>(`/tenants/${id}/guardrails`, {
      method: "PATCH",
      body: JSON.stringify({ policy }),
    }),
  deleteTenant: (id: string) =>
    api<void>(`/tenants/${id}`, { method: "DELETE" }),
  listKeys: (tenantId?: string) =>
    api<ApiKey[]>(`/keys${tenantId ? `?tenant_id=${tenantId}` : ""}`, {
      next: { revalidate: LIST_REVALIDATE_SECS, tags: [CACHE_TAGS.keys] },
    }),
  createKey: (
    tenantId: string,
    body: {
      name: string;
      description?: string;
      budget_tokens?: number | null;
      budget_cost_usd?: number | null;
      budget_period?: string | null;
      budget_started_at?: string | null;
    },
  ) =>
    api<CreatedKey>(`/tenants/${tenantId}/keys`, {
      method: "POST",
      body: JSON.stringify(body),
    }),
  updateKey: (
    id: string,
    body: {
      name: string;
      description?: string;
      budget_tokens?: number | null;
      budget_cost_usd?: number | null;
      budget_period?: string | null;
      budget_started_at?: string | null;
    },
  ) =>
    api<ApiKey>(`/keys/${id}`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  setKeyDisabled: (id: string, disabled: boolean) =>
    api<void>(`/keys/${id}/disabled`, {
      method: "PUT",
      body: JSON.stringify({ disabled }),
    }),
  setKeyTracing: (id: string, tracing_enabled: boolean) =>
    api<void>(`/keys/${id}/tracing`, {
      method: "PUT",
      body: JSON.stringify({ tracing_enabled }),
    }),
  setTenantTracing: (id: string, tracing_enabled: boolean) =>
    api<void>(`/tenants/${id}/tracing`, {
      method: "PUT",
      body: JSON.stringify({ tracing_enabled }),
    }),
  deleteKey: (id: string) => api<void>(`/keys/${id}`, { method: "DELETE" }),
  listModels: () =>
    api<ModelRoute[]>("/models", {
      next: { revalidate: LIST_REVALIDATE_SECS, tags: [CACHE_TAGS.models] },
    }),
  createModel: (
    body: Partial<ModelRoute> & {
      model_name: string;
      upstream_model: string;
      api_base: string;
    },
  ) =>
    api<ModelRoute>("/models", { method: "POST", body: JSON.stringify(body) }),
  updateModel: (
    id: string,
    body: Partial<ModelRoute> & { upstream_model: string; api_base: string },
  ) =>
    api<ModelRoute>(`/models/${id}`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  deleteModel: (id: string) => api<void>(`/models/${id}`, { method: "DELETE" }),
  modelHealth: () => api<ModelHealthSummary[]>("/models/health"),
  modelHealthDetail: (id: string) =>
    api<ModelHealthDetail>(`/models/${id}/health`),
  checkModelHealth: (id: string) =>
    api<ModelHealthDetail>(`/models/${id}/health/check`, { method: "POST" }),
  checkAllModelHealth: () =>
    api<BulkModelHealthResult>("/models/health/check", { method: "POST" }),
  setModelHealthConfig: (id: string, body: ModelHealthConfigBody) =>
    api<ModelHealthSummary>(`/models/${id}/health/config`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  setModelWeight: (id: string, admission_weight: number) =>
    api<ModelRoute>(`/models/${id}/weight`, {
      method: "PUT",
      body: JSON.stringify({ admission_weight }),
    }),
  setModelCapacity: (id: string, max_in_flight: number | null) =>
    api<ModelRoute>(`/models/${id}/capacity`, {
      method: "PUT",
      body: JSON.stringify({ max_in_flight }),
    }),
  setModelCapacityMode: (id: string, capacity_mode: string) =>
    api<ModelRoute>(`/models/${id}/capacity-mode`, {
      method: "PUT",
      body: JSON.stringify({ capacity_mode }),
    }),
  autotuneModel: (
    id: string,
    opts?: {
      workload?: AutotuneWorkload;
      latency_headroom?: number;
      replicas?: number;
    },
  ) =>
    api<AutotuneReport>(`/models/${id}/autotune`, {
      method: "POST",
      body: JSON.stringify(opts ?? {}),
    }),
  applyAutotuneCapacity: (id: string, max_in_flight: number) =>
    api<ModelRoute>(`/models/${id}/autotune/apply`, {
      method: "POST",
      body: JSON.stringify({ max_in_flight }),
    }),
  setModelCache: (
    id: string,
    cache_enabled: boolean,
    cache_ttl_secs?: number,
  ) =>
    api<ModelRoute>(`/models/${id}/cache`, {
      method: "PUT",
      body: JSON.stringify({ cache_enabled, cache_ttl_secs }),
    }),
  setModelReliability: (
    id: string,
    body: {
      request_timeout_secs: number | null;
      max_retries: number;
      retry_backoff_ms: number;
      endpoint_selection_mode: string;
    },
  ) =>
    api<ModelRoute>(`/models/${id}/reliability`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  listModelEndpoints: (id: string) =>
    api<ModelEndpoint[]>(`/models/${id}/endpoints`),
  getManagedModel: (id: string) =>
    api<ManagedModelSpec | null>(`/models/${id}/managed`),
  putManagedModel: (id: string, body: PutManagedModel) =>
    api<ManagedModelSpec>(`/models/${id}/managed`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  deleteManagedModel: (id: string) =>
    api<void>(`/models/${id}/managed`, { method: "DELETE" }),
  slurmResources: () => api<ClusterResources>(`/slurm/resources`),
  listRecipes: () => api<SavedRecipe[]>(`/recipes`),
  createRecipe: (body: Omit<SavedRecipe, "id">) =>
    api<SavedRecipe>(`/recipes`, { method: "POST", body: JSON.stringify(body) }),
  updateRecipe: (id: string, body: Omit<SavedRecipe, "id">) =>
    api<SavedRecipe>(`/recipes/${id}`, { method: "PUT", body: JSON.stringify(body) }),
  deleteRecipe: (id: string) =>
    api<void>(`/recipes/${id}`, { method: "DELETE" }),
  listReplicas: (id: string) =>
    api<ModelReplica[]>(`/models/${id}/replicas`),
  createModelEndpoint: (
    id: string,
    body: {
      name: string;
      api_base: string;
      api_key?: string | null;
      priority?: number;
      weight?: number;
      enabled?: boolean;
    },
  ) =>
    api<ModelEndpoint>(`/models/${id}/endpoints`, {
      method: "POST",
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
    },
  ) =>
    api<ModelEndpoint>(`/models/${id}/endpoints/${endpointId}`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  deleteModelEndpoint: (id: string, endpointId: string) =>
    api<void>(`/models/${id}/endpoints/${endpointId}`, { method: "DELETE" }),
  cacheStats: (sinceMs?: number) =>
    api<CacheStats>(`/usage/cache${qs({ since_ms: sinceMs })}`),
  listMcpServers: () => api<McpServer[]>("/mcp-servers"),
  createMcpServer: (body: {
    name: string;
    upstream_url: string;
    auth_header?: string;
  }) =>
    api<McpServer>("/mcp-servers", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  updateMcpServer: (
    id: string,
    body: { upstream_url: string; auth_header?: string; enabled?: boolean },
  ) =>
    api<McpServer>(`/mcp-servers/${id}`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  deleteMcpServer: (id: string) =>
    api<void>(`/mcp-servers/${id}`, { method: "DELETE" }),
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
        status: params.status,
        request_id: params.requestId,
        since_ms: params.sinceMs,
        until_ms: params.untilMs,
        before_ms: params.beforeMs,
        before_request_id: params.beforeRequestId,
        limit: params.limit,
        traced_only: params.tracedOnly ? "true" : undefined,
      })}`,
    ),
  getRequestSpans: (requestId: string) =>
    api<SpanEntry[]>(`/usage/logs/${requestId}/spans`).catch(() => [] as SpanEntry[]),
  // Server-only: hand the control-plane (Charo's model-test console) the reserved
  // system key secret so it can call the data plane as the protected internal tenant.
  controlPlaneKey: () =>
    api<{ secret: string }>("/system/control-plane-key"),
  getUsageRetention: () => api<UsageRetentionView>("/settings/usage-retention"),
  setUsageRetention: (days: number) =>
    api<UsageRetentionView>("/settings/usage-retention", {
      method: "PUT",
      body: JSON.stringify({ days }),
    }),
  compactUsage: () =>
    api<CompactUsageResult>("/usage/compact", { method: "POST" }),
  stats: () => api<LiveStats>("/stats"),
  fairshareLive: () => api<FairshareLiveView>("/fairshare/live"),
  audit: (limit = 100) => api<AuditEntry[]>(`/audit?limit=${limit}`),
  getCapacity: () => api<{ max_in_flight: number }>("/capacity"),
  setCapacity: (max_in_flight: number) =>
    api<{ max_in_flight: number }>("/capacity", {
      method: "PUT",
      body: JSON.stringify({ max_in_flight }),
    }),
  getAlertSettings: () => api<AlertSettingsView>("/settings/alerts"),
  setAlertSettings: (body: UpdateAlertSettings) =>
    api<AlertSettingsView>("/settings/alerts", {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  testAlert: () =>
    api<TestAlertResult>("/settings/alerts/test", { method: "POST" }),
  getAutoRouterSettings: () =>
    api<AutoRouterSettingsView>("/settings/auto-router"),
  setAutoRouterSettings: (body: UpdateAutoRouterSettings) =>
    api<AutoRouterSettingsView>("/settings/auto-router", {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  getBoonSettings: () => api<BoonSettingsView>("/settings/boons"),
  setBoonSettings: (body: UpdateBoonSettings) =>
    api<BoonSettingsView>("/settings/boons", {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  getCharoSettings: () => api<CharoSettingsView>("/settings/charo"),
  setCharoSettings: (body: CharoSettingsView) =>
    api<CharoSettingsView>("/settings/charo", {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  getSlurmSettings: () => api<SlurmSettingsView>("/settings/slurm"),
  setSlurmSettings: (body: UpdateSlurmSettings) =>
    api<SlurmSettingsView>("/settings/slurm", {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  testSlurmConnection: () =>
    api<SlurmHealthView>("/settings/slurm/test", { method: "POST" }),
  exportBackup: () => api<ConfigBackup>("/backup/export"),
  restoreBackup: (body: ConfigBackup) =>
    api<RestoreReport>("/backup/restore", {
      method: "POST",
      body: JSON.stringify(body),
    }),
};
