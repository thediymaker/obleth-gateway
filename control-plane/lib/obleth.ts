// Server-side client for the obleth Management API.

const BASE = process.env.OBLETH_ADMIN_BASE_URL ?? "http://localhost:9090";

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

export interface Tenant {
  id: string;
  name: string;
  fairshare_group: string;
  weight: number;
  tokens_per_minute: number;
  max_in_flight: number | null;
  created_at: string;
  updated_at: string;
}

export interface ApiKey {
  id: string;
  tenant_id: string;
  name: string;
  key_prefix: string;
  disabled: boolean;
  created_at: string;
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
  input_cost_per_token: number;
  output_cost_per_token: number;
  context_window: number;
  admission_weight: number;
  max_in_flight: number | null;
  supports_function_calling: boolean;
  supports_system_messages: boolean;
  supports_response_schema: boolean;
  supports_tool_choice: boolean;
  enabled: boolean;
  cache_enabled: boolean;
  cache_ttl_secs: number;
  created_at: string;
  updated_at: string;
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

export interface UsageModelAgg {
  model: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
}

export interface UsageTimePoint {
  bucket_ms: number;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
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
}

export interface TenantUsageTimePoint {
  tenant_id: string;
  bucket_ms: number;
  requests: number;
  total_tokens: number;
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

async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}/api/v1${path}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${adminToken()}`,
      "Content-Type": "application/json",
      ...(init?.headers ?? {}),
    },
    cache: "no-store",
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`obleth api ${path} -> ${res.status}: ${text}`);
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

export const obleth = {
  listTenants: () => api<Tenant[]>("/tenants"),
  createTenant: (body: {
    name: string;
    weight?: number;
    tokens_per_minute?: number;
    max_in_flight?: number | null;
  }) => api<Tenant>("/tenants", { method: "POST", body: JSON.stringify(body) }),
  setWeight: (id: string, weight: number) =>
    api<Tenant>(`/tenants/${id}/weight`, { method: "PATCH", body: JSON.stringify({ weight }) }),
  setQuota: (id: string, tokens_per_minute: number, max_in_flight: number | null) =>
    api<Tenant>(`/tenants/${id}/quota`, {
      method: "PUT",
      body: JSON.stringify({ tokens_per_minute, max_in_flight }),
    }),
  listKeys: (tenantId?: string) =>
    api<ApiKey[]>(`/keys${tenantId ? `?tenant_id=${tenantId}` : ""}`),
  createKey: (tenantId: string, name: string) =>
    api<CreatedKey>(`/tenants/${tenantId}/keys`, {
      method: "POST",
      body: JSON.stringify({ name }),
    }),
  setKeyDisabled: (id: string, disabled: boolean) =>
    api<void>(`/keys/${id}/disabled`, { method: "PUT", body: JSON.stringify({ disabled }) }),
  deleteKey: (id: string) => api<void>(`/keys/${id}`, { method: "DELETE" }),
  listModels: () => api<ModelRoute[]>("/models"),
  createModel: (body: Partial<ModelRoute> & { model_name: string; upstream_model: string; api_base: string }) =>
    api<ModelRoute>("/models", { method: "POST", body: JSON.stringify(body) }),
  updateModel: (id: string, body: Partial<ModelRoute> & { upstream_model: string; api_base: string }) =>
    api<ModelRoute>(`/models/${id}`, { method: "PUT", body: JSON.stringify(body) }),
  deleteModel: (id: string) => api<void>(`/models/${id}`, { method: "DELETE" }),
  modelHealth: () => api<ModelHealthSummary[]>("/models/health"),
  modelHealthDetail: (id: string) => api<ModelHealthDetail>(`/models/${id}/health`),
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
  setModelCache: (id: string, cache_enabled: boolean, cache_ttl_secs?: number) =>
    api<ModelRoute>(`/models/${id}/cache`, {
      method: "PUT",
      body: JSON.stringify({ cache_enabled, cache_ttl_secs }),
    }),
  cacheStats: (sinceMs?: number) => api<CacheStats>(`/usage/cache${qs({ since_ms: sinceMs })}`),
  listMcpServers: () => api<McpServer[]>("/mcp-servers"),
  createMcpServer: (body: { name: string; upstream_url: string; auth_header?: string }) =>
    api<McpServer>("/mcp-servers", { method: "POST", body: JSON.stringify(body) }),
  updateMcpServer: (
    id: string,
    body: { upstream_url: string; auth_header?: string; enabled?: boolean },
  ) => api<McpServer>(`/mcp-servers/${id}`, { method: "PUT", body: JSON.stringify(body) }),
  deleteMcpServer: (id: string) => api<void>(`/mcp-servers/${id}`, { method: "DELETE" }),
  usage: (sinceMs?: number) => api<UsageAgg[]>(`/usage${qs({ since_ms: sinceMs })}`),
  usageByKey: (sinceMs?: number, limit?: number) =>
    api<UsageKeyAgg[]>(`/usage/keys${qs({ since_ms: sinceMs, limit })}`),
  usageByModel: (sinceMs?: number) => api<UsageModelAgg[]>(`/usage/models${qs({ since_ms: sinceMs })}`),
  usageSeries: (bucketMs = 300_000, sinceMs?: number) =>
    api<UsageTimePoint[]>(`/usage/series${qs({ bucket_ms: bucketMs, since_ms: sinceMs })}`),
  usageSeriesByTenant: (bucketMs = 10_000, sinceMs?: number) =>
    api<TenantUsageTimePoint[]>(`/usage/series/tenants${qs({ bucket_ms: bucketMs, since_ms: sinceMs })}`),
  costs: (sinceMs?: number) => api<CostAgg[]>(`/costs${qs({ since_ms: sinceMs })}`),
  stats: () => api<LiveStats>("/stats"),
  fairshareLive: () => api<FairshareLiveView>("/fairshare/live"),
  audit: (limit = 100) => api<AuditEntry[]>(`/audit?limit=${limit}`),
  getCapacity: () => api<{ max_in_flight: number }>("/capacity"),
  setCapacity: (max_in_flight: number) =>
    api<{ max_in_flight: number }>("/capacity", {
      method: "PUT",
      body: JSON.stringify({ max_in_flight }),
    }),
};
