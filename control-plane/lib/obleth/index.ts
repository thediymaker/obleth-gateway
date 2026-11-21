// Server-side client for the obleth Management API.

// React's request-scoped memo: no-arg settings getters are wrapped in
// `reactCache` so a layout and page reading the same setting in one render
// tree cost one admin round-trip. Freshness is unchanged — the memo does not
// outlive the request.
import { keyQueryParams, type KeyListQuery } from "@/lib/key-query";
import { cache as reactCache } from "react";
import { keyWatchQuery, usageQs, OblethApiError, API_TIMEOUT_MS, type AuditOptions, auditActorHeaders, api, apiStream, seg, qs, logFilterQuery } from "./client";
import type { WeeklyWindow, GuardrailsPolicy, CompressionPolicy, Tenant, ApiKey, CreatedKey, LifecycleStatus, SetModelStatus, ModelReasoning, ModelFamily, ModelFamilyWrite, ModelRoute, ModelWriteFields, ModelEndpoint, CapacityDiscoveryFields, CapacityDiscoveryView, CapacityServicesView, ManagedModelSpec, PutManagedModel, ClusterResources, ModelReplica, DeploymentLaunch, DeploymentLaunchesParams, AutotuneWorkload, AutotuneReport, ModelHealthSummary, ModelHealthDetail, ModelActivation, ValidateModelBody, ValidateModelResult, ModelHealthConfigBody, CacheStats, McpServer, UsageAgg, KeyStats, BudgetUsage, KeySource, KeyRejectionAgg, KeyWatchParams, KeyUsageSummary, UsageModelAgg, UsageDailyRow, UsageLogEntry, SpanEntry, UsageLogParams, UsageLogFacets, UsageLogHistogram, UsageFailureGroups, UsageRetentionView, KeyPrefixView, NamedNetwork, NetworkSettingsView, CompactUsageResult, ResyncReport, ModelSpeedPoint, UsageSpeedParams, UsageDailyParams, LiveStats, OverviewView, OverviewSummaryView, FairshareGroup, FairshareLiveView, FairshareHistoryView, TenantUsageTimePoint, ModelUsageTimePoint, UsageBreakdownEntry, UsageFilter, AuditParams, DailyStatsView, AuditEntry, AlertSettingsView, UpdateAlertSettings, SlurmSettingsView, UpdateSlurmSettings, SlurmHealthView, AutoRouterSettingsView, UpdateAutoRouterSettings, RouterReadinessView, SnoozedAdviceView, AdviceReportView, RouteExplainView, SimulateRouteRequest, BoonSettingsView, UpdateBoonSettings, CompressorStatusView, CharoSettingsView, EnergySettingsView, UpdateEnergySettings, EnergyTestQuery, EnergyTestResult, ModelManifest, ModelImportReport, RestoreReport, TestAlertResult, Recipe, KnowledgeCollection, KnowledgeDocument, KnowledgeHit, KnowledgeReindexResult, ModelKnowledgeCollections, KnowledgeSettingsView, UpdateKnowledgeSettings, AdminBatch, AdminBatchList, BatchListParams, VersionInfo } from "./types";

export * from "./types";
export { API_TIMEOUT_MS, OblethApiError, seg } from "./client";

export const obleth = {
  gatewayVersion: () =>
    api<VersionInfo>("/version", { next: { revalidate: 300 } }),
  listTenants: () => api<Tenant[]>("/tenants"),
  getTenant: (id: string) => api<Tenant>(`/tenants/${seg(id)}`),
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
    api<Tenant>(`/tenants/${seg(id)}/weight`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ weight }),
    }),
  listFairshareGroups: () => api<FairshareGroup[]>("/fairshare/groups"),
  createFairshareGroup: (name: string, weight = 100, options?: AuditOptions) =>
    api<FairshareGroup>("/fairshare/groups", {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ name, weight }),
    }),
  setFairshareGroupWeight: (name: string, weight: number, options?: AuditOptions) =>
    api<FairshareGroup>(`/fairshare/groups/${seg(name)}/weight`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ weight }),
    }),
  setQuota: (
    id: string,
    tokens_per_minute: number,
    max_in_flight: number | null,
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${seg(id)}/quota`, {
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
    api<Tenant>(`/tenants/${seg(id)}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  setTenantStatus: (id: string, status: string, options?: AuditOptions) =>
    api<Tenant>(`/tenants/${seg(id)}/status`, {
      method: "PUT",
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
    api<Tenant>(`/tenants/${seg(id)}/schedule`, {
      method: "PUT",
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
    api<Tenant>(`/tenants/${seg(id)}/budget`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  setTenantAllowlist: (
    id: string,
    allowed_models: string[],
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${seg(id)}/allowlist`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ allowed_models }),
    }),
  setTenantGuardrails: (
    id: string,
    policy: GuardrailsPolicy | null,
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${seg(id)}/guardrails`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ policy }),
    }),
  setTenantCompression: (
    id: string,
    policy: CompressionPolicy | null,
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${seg(id)}/compression`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ policy }),
    }),
  /** Move a tenant into another fairshare group (one with no weight shares at the default). */
  setTenantGroup: (id: string, fairshare_group: string, options?: AuditOptions) =>
    api<Tenant>(`/tenants/${seg(id)}/group`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ fairshare_group }),
    }),
  deleteTenant: (id: string, options?: AuditOptions) =>
    api<void>(`/tenants/${seg(id)}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  listKeys: (tenantId?: string) => api<ApiKey[]>(`/keys${qs({ tenant_id: tenantId })}`),
  /**
   * Every key (or one tenant's) with only the named fields: what a picker or
   * a count needs, without each key's settings. An older gateway ignores
   * `fields` and sends whole keys, which still fit the type.
   */
  listKeyFields: <F extends keyof ApiKey>(fields: readonly F[], tenantId?: string) =>
    api<Pick<ApiKey, F>[]>(`/keys${qs({ fields: fields.join(","), tenant_id: tenantId })}`),
  /**
   * One page of the keys `query` matches, in its order, and where the next
   * page starts (`null` on the last). `fields` sends only those fields.
   */
  listKeysPage: async <F extends keyof ApiKey = keyof ApiKey>(
    query: KeyListQuery,
    page: { limit: number; cursor?: string; fields?: readonly F[] },
  ) => {
    let next: string | null = null;
    const data = await api<Pick<ApiKey, F>[]>(
      `/keys${qs({ ...keyQueryParams(query), limit: page.limit, cursor: page.cursor || undefined, fields: page.fields?.join(",") })}`,
      { onHeaders: (h) => { next = h.get("x-obleth-next-cursor"); } },
    );
    return { data, next: next as string | null };
  },
  /** How many keys `query` matches (its order aside), and in how many tenants. */
  countKeys: (query: KeyListQuery) => api<{ keys: number; tenants: number }>(`/keys/count${qs(keyQueryParams(query))}`),
  /** Every key's counts for the Keys page's tiles. */
  keyStats: (window: { sinceMs: number; untilMs: number; recentMs: number }) =>
    api<KeyStats>(`/keys/stats${qs({ since_ms: window.sinceMs, until_ms: window.untilMs, recent_ms: window.recentMs })}`),
  /** One key, or a 404 `OblethApiError`. */
  getKey: (id: string) => api<ApiKey>(`/keys/${encodeURIComponent(id)}`),
  /** The keys with these ids that exist, oldest first; 200 ids per call at most. */
  keysByIds: (ids: string[]) => api<ApiKey[]>(`/keys${qs({ ids: ids.join(",") })}`),
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
      /** CIDR ranges or addresses; empty for any network. */
      allowed_networks?: string[];
      /** Two-letter country codes; empty for none. */
      allowed_countries?: string[];
    },
    options?: AuditOptions,
  ) =>
    api<CreatedKey>(`/tenants/${seg(tenantId)}/keys`, {
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
      /** Left out, the key keeps its list; `[]` allows any network. */
      allowed_networks?: string[];
      /** Left out, the key keeps its list; `[]` clears it. */
      allowed_countries?: string[];
    },
    options?: AuditOptions,
  ) =>
    api<ApiKey>(`/keys/${seg(id)}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  /** Move a key to another tenant, keeping its secret and settings. */
  moveKey: (id: string, tenant_id: string, options?: AuditOptions) =>
    api<ApiKey>(`/keys/${seg(id)}/tenant`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ tenant_id }),
    }),
  /** Every capped tenant and key budget, with its current period's use from the enforcement counters. */
  /** Every capped budget, or one tenant's own and its keys'. */
  budgetUsage: (tenantId?: string) => api<BudgetUsage[]>(`/budgets/usage${qs({ tenant_id: tenantId })}`),
  /** These keys' budgets only (up to 200 ids). */
  keyBudgets: (ids: string[]) => api<BudgetUsage[]>(`/budgets/usage${qs({ ids: ids.join(",") })}`),
  setKeyDisabled: (id: string, disabled: boolean, options?: AuditOptions) =>
    api<ApiKey>(`/keys/${seg(id)}/disabled`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ disabled }),
    }),
  setKeyTracing: (id: string, tracing_enabled: boolean, options?: AuditOptions) =>
    api<ApiKey>(`/keys/${seg(id)}/tracing`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ tracing_enabled }),
    }),
  setKeyEndUserFairshare: (id: string, end_user_fairshare: boolean, options?: AuditOptions) =>
    api<ApiKey>(`/keys/${seg(id)}/end-user-fairshare`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ end_user_fairshare }),
    }),
  setKeyBackground: (id: string, background: boolean, options?: AuditOptions) =>
    api<ApiKey>(`/keys/${seg(id)}/background`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ background }),
    }),
  setTenantTracing: (
    id: string,
    tracing_enabled: boolean,
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${seg(id)}/tracing`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ tracing_enabled }),
    }),
  setTenantSynthetic: (
    id: string,
    synthetic: boolean,
    options?: AuditOptions,
  ) =>
    api<Tenant>(`/tenants/${seg(id)}/synthetic`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ synthetic }),
    }),
  deleteKey: (id: string, options?: AuditOptions) =>
    api<void>(`/keys/${seg(id)}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  listModels: () => api<ModelRoute[]>("/models"),
  getModel: (id: string) => api<ModelRoute>(`/models/${seg(id)}`),
  createModel: (
    body: ModelWriteFields & {
      model_name: string;
      upstream_model: string;
      api_base: string;
      /** `staged` keeps it out of the model list and auto until it is made active. Omitted = active. */
      status?: Extract<LifecycleStatus, "staged" | "active">;
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
    api<ModelRoute>(`/models/${seg(id)}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteModel: (id: string, options?: AuditOptions) =>
    api<void>(`/models/${seg(id)}`, {
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
    api<ModelHealthDetail>(`/models/${seg(id)}/health`),
  checkModelHealth: (id: string) =>
    api<ModelHealthDetail>(`/models/${seg(id)}/health/check`, { method: "POST" }),
  /** Probe a switched-off model and turn it on if the probe comes back healthy. */
  activateModel: (id: string, options?: AuditOptions) =>
    api<ModelActivation>(`/models/${seg(id)}/health/activate`, {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  setModelHealthConfig: (
    id: string,
    body: ModelHealthConfigBody,
    options?: AuditOptions,
  ) =>
    api<ModelHealthSummary>(`/models/${seg(id)}/health/config`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  setModelStatus: (id: string, body: SetModelStatus, options?: AuditOptions) =>
    api<ModelRoute>(`/models/${seg(id)}/status`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  /** Replace a model's reasoning-effort map whole. */
  setModelReasoning: (id: string, body: ModelReasoning, options?: AuditOptions) =>
    api<ModelRoute>(`/models/${seg(id)}/reasoning`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  setModelListing: (id: string, listed: boolean, options?: AuditOptions) =>
    api<ModelRoute>(`/models/${seg(id)}/listing`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ listed }),
    }),
  listModelFamilies: () => api<ModelFamily[]>("/model-families"),
  createModelFamily: (body: ModelFamilyWrite, options?: AuditOptions) =>
    api<ModelFamily>("/model-families", {
      method: "POST",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  updateModelFamily: (id: string, body: ModelFamilyWrite, options?: AuditOptions) =>
    api<ModelFamily>(`/model-families/${seg(id)}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteModelFamily: (id: string, options?: AuditOptions) =>
    api<void>(`/model-families/${seg(id)}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  setModelCapacity: (
    id: string,
    max_in_flight: number | null,
    options?: AuditOptions,
  ) =>
    api<ModelRoute>(`/models/${seg(id)}/capacity`, {
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
    api<ModelRoute>(`/models/${seg(id)}/capacity-mode`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ capacity_mode, ...(fields ?? {}) }),
    }),
  capacityDiscovery: () => api<CapacityDiscoveryView>("/capacity/discovery"),
  capacityServices: (model?: string) =>
    api<CapacityServicesView>(`/capacity/services${model ? `?model=${encodeURIComponent(model)}` : ""}`),
  autotuneModel: (
    id: string,
    opts?: {
      workload?: AutotuneWorkload;
      latency_headroom?: number;
      replicas?: number;
    },
    options?: AuditOptions,
  ) =>
    api<AutotuneReport>(`/models/${seg(id)}/autotune`, {
      method: "POST",
      timeoutMs: API_TIMEOUT_MS.long,
      headers: auditActorHeaders(options),
      body: JSON.stringify(opts ?? {}),
    }),
  applyAutotuneCapacity: (
    id: string,
    max_in_flight: number,
    options?: AuditOptions,
  ) =>
    api<ModelRoute>(`/models/${seg(id)}/autotune/apply`, {
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
    api<ModelRoute>(`/models/${seg(id)}/cache`, {
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
    api<ModelRoute>(`/models/${seg(id)}/reliability`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  listModelEndpoints: (id: string) =>
    api<ModelEndpoint[]>(`/models/${seg(id)}/endpoints`),
  getManagedModel: (id: string) =>
    api<ManagedModelSpec | null>(`/models/${seg(id)}/managed`),
  listManagedModels: () => api<ManagedModelSpec[]>("/managed"),
  putManagedModel: (
    id: string,
    body: PutManagedModel,
    options?: AuditOptions,
  ) =>
    api<ManagedModelSpec>(`/models/${seg(id)}/managed`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteManagedModel: (id: string, options?: AuditOptions) =>
    api<void>(`/models/${seg(id)}/managed`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  clearProvisionError: (id: string, options?: AuditOptions) =>
    api<{ ok: boolean }>(`/models/${seg(id)}/managed/provision-error`, {
      method: "PATCH",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ error: null }),
    }),
  /** Clear a failure pause and its backoff, so the next pass submits. */
  retryManagedModel: (id: string, options?: AuditOptions) =>
    api<unknown>(`/models/${seg(id)}/managed/retry`, {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  /** Replace every live replica one at a time, each new job started before the old one is stopped. */
  rollManagedModel: (id: string, options?: AuditOptions) =>
    api<unknown>(`/models/${seg(id)}/managed/roll`, {
      method: "POST",
      headers: auditActorHeaders(options),
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
    api<Recipe>(`/recipes/${seg(id)}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteRecipe: (id: string, options?: AuditOptions) =>
    api<void>(`/recipes/${seg(id)}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  /** Every replica of every managed model. */
  listAllReplicas: () => api<ModelReplica[]>("/replicas"),
  listReplicas: (id: string) =>
    api<ModelReplica[]>(`/models/${seg(id)}/replicas`),
  clearLostReplicas: (id: string, options?: AuditOptions) =>
    api<{ deleted: number }>(`/models/${seg(id)}/replicas/clear-lost`, {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  /** Launch history of Slurm-backed replicas, newest first (limit default 50, max 500). */
  listDeploymentLaunches: (params: DeploymentLaunchesParams = {}) =>
    api<DeploymentLaunch[]>(`/deployments/launches${qs({ ...params })}`),
  restartReplica: (replicaId: string, options?: AuditOptions) =>
    api<{ ok: boolean }>(`/replicas/${seg(replicaId)}/restart`, {
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
    api<ModelEndpoint>(`/models/${seg(id)}/endpoints`, {
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
    api<ModelEndpoint>(`/models/${seg(id)}/endpoints/${seg(endpointId)}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteModelEndpoint: (id: string, endpointId: string, options?: AuditOptions) =>
    api<void>(`/models/${seg(id)}/endpoints/${seg(endpointId)}`, {
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
    body: { upstream_url: string; auth_header?: string; clear_auth?: boolean; enabled?: boolean; name?: string; allowed_tenants?: string[] },
    options?: AuditOptions,
  ) =>
    api<McpServer>(`/mcp-servers/${seg(id)}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteMcpServer: (id: string, options?: AuditOptions) =>
    api<void>(`/mcp-servers/${seg(id)}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  usage: (filter: UsageFilter = {}) =>
    api<UsageAgg[]>(`/usage${usageQs(filter)}`),
  usageKeysSummary: (
    params: { tenantId?: string; keyIds?: string[]; sinceMs?: number; untilMs?: number; limit?: number } = {},
  ) =>
    api<KeyUsageSummary[]>(
      `/usage/keys/summary${qs({
        tenant_id: params.tenantId,
        key_id: params.keyIds?.join(","),
        since_ms: params.sinceMs,
        until_ms: params.untilMs,
        limit: params.limit,
      })}`,
    ),
  usageByModel: (filter: UsageFilter = {}) =>
    api<UsageModelAgg[]>(`/usage/models${usageQs(filter)}`),
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
  usageSpeed: (params: UsageSpeedParams) =>
    api<ModelSpeedPoint[]>(
      `/usage/speed${qs({
        since_ms: params.sinceMs,
        until_ms: params.untilMs,
        bucket: params.bucket,
        tz: params.tz,
        tenant_id: params.tenantId,
        key_id: params.keyId,
        model: params.model,
      })}`,
    ),
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
        ...logFilterQuery(params),
        before_ms: params.beforeMs,
        before_request_id: params.beforeRequestId,
        limit: params.limit,
        parent_request_id: params.parentRequestId,
      })}`,
    ),
  /** Requests and failures per bucket, counted with the log's own filters. */
  usageLogHistogram: (params: UsageLogParams & { bucketMs?: number } = {}) =>
    api<UsageLogHistogram>(
      `/usage/logs/histogram${qs({ ...logFilterQuery(params), bucket_ms: params.bucketMs })}`,
    ),
  /** What the requests matching the log's filters are made of, busiest first. */
  usageLogFacets: (params: UsageLogParams = {}) =>
    api<UsageLogFacets>(`/usage/logs/facets${qs(logFilterQuery(params))}`),
  /** The failures among the requests matching the log's filters, grouped into repeats; the last 24 hours by default. */
  /** Where each key was used from in a window: one row per key, network and country. */
  keySources: (params: KeyWatchParams = {}) => api<KeySource[]>(`/usage/key-sources${keyWatchQuery(params)}`),
  /** Refused uses of real keys in a window: turned off, or outside the key's networks. */
  keyRejections: (params: KeyWatchParams = {}) => api<KeyRejectionAgg[]>(`/usage/key-rejections${keyWatchQuery(params)}`),
  usageFailures: (params: UsageLogParams = {}) =>
    api<UsageFailureGroups>(`/usage/failures${qs(logFilterQuery(params))}`),
  getRequestSpans: (requestId: string) =>
    api<SpanEntry[]>(`/usage/logs/${seg(requestId)}/spans`).catch(() => [] as SpanEntry[]),
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
  getKeyPrefix: reactCache(() => api<KeyPrefixView>("/settings/key-prefix")),
  getNetworkSettings: reactCache(() => api<NetworkSettingsView>("/settings/networks")),
  setNamedNetworks: (networks: NamedNetwork[], options?: AuditOptions) =>
    api<NetworkSettingsView>("/settings/networks", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ networks }),
    }),
  setKeyPrefix: (prefix: string, options?: AuditOptions) =>
    api<KeyPrefixView>("/settings/key-prefix", {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify({ prefix }),
    }),
  compactUsage: (options?: AuditOptions) =>
    api<CompactUsageResult>("/usage/compact", {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  resync: (options?: AuditOptions) =>
    api<ResyncReport>("/resync", {
      method: "POST",
      timeoutMs: API_TIMEOUT_MS.long,
      headers: auditActorHeaders(options),
    }),
  stats: () => api<LiveStats>("/stats"),
  /** The Overview's window, the one before it, series and breakdowns, in one read. */
  overview: (sinceMs: number, bucketMs: number) =>
    api<OverviewView>(`/overview${qs({ since_ms: sinceMs, bucket_ms: bucketMs })}`),
  overviewSummary: (sinceMs?: number, untilMs?: number) =>
    api<OverviewSummaryView>(`/overview/summary${qs({ since_ms: sinceMs, until_ms: untilMs })}`),
  fairshareLive: () => api<FairshareLiveView>("/fairshare/live"),
  fairshareHistory: (params: { since_ms?: number; model?: string } = {}) =>
    api<FairshareHistoryView>(
      `/fairshare/history${qs({ since_ms: params.since_ms, model: params.model })}`,
    ),
  /** Batches across every tenant, newest first, with their progress. */
  listBatches: (params: BatchListParams = {}) =>
    api<AdminBatchList>(
      `/batches${qs({ status: params.status, tenant_id: params.tenantId, model: params.model, after: params.after, limit: params.limit })}`,
    ),
  /** Stops a batch's queued requests; running ones finish. */
  cancelBatch: (id: string, options?: AuditOptions) =>
    api<AdminBatch>(`/batches/${seg(id)}/cancel`, {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  audit: (limit = 100) => api<AuditEntry[]>(`/audit?limit=${limit}`),
  /** The audit log narrowed by who, what and when; page back with `before_id`. */
  auditQuery: (params: AuditParams = {}) =>
    api<AuditEntry[]>(
      `/audit${qs({
        limit: params.limit,
        actor: params.actor,
        entity_type: params.entityType,
        entity_id: params.entityId,
        action: params.action,
        since: params.since,
        until: params.until,
        before_id: params.beforeId,
        q: params.q,
      })}`,
    ),
  /** Daily counters the proxy keeps: MCP calls per server, knowledge searches per collection. */
  dailyStats: (kind: "mcp" | "knowledge", days = 7) => api<DailyStatsView>(`/stats/daily${qs({ kind, days })}`),
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
  /// Advice from the last day of requests (cached about a minute by the gateway).
  getAdvice: reactCache(() => api<AdviceReportView>("/advice")),
  snoozeAdvice: (key: string, body: { title: string; until_ms?: number }, options?: AuditOptions) =>
    api<SnoozedAdviceView[]>(`/advice/snoozed/${seg(key)}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  restoreAdvice: (key: string, options?: AuditOptions) =>
    api<SnoozedAdviceView[]>(`/advice/snoozed/${seg(key)}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
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
  testEnergyQuery: (query: EnergyTestQuery) =>
    api<EnergyTestResult>("/settings/energy/test", {
      method: "POST",
      body: JSON.stringify(query),
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
  /** The config backup as the gateway's own response, to stream to the browser. */
  exportBackup: (options?: AuditOptions) =>
    apiStream("/backup/export", {
      timeoutMs: API_TIMEOUT_MS.long,
      headers: auditActorHeaders(options),
    }),
  /** Restores a backup from its file's text, sent as it is (not re-serialised). */
  restoreBackup: (text: string, options?: AuditOptions) =>
    api<RestoreReport>("/backup/restore", {
      method: "POST",
      timeoutMs: API_TIMEOUT_MS.long,
      headers: auditActorHeaders(options),
      body: text,
    }),
  /** The model manifest; `names` limits it to those models (unknown names are skipped). */
  exportModels: (names: string[] = [], options?: AuditOptions) =>
    api<ModelManifest>(`/models/export${qs({ names: names.length ? names.join(",") : undefined })}`, {
      timeoutMs: API_TIMEOUT_MS.long,
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
        timeoutMs: API_TIMEOUT_MS.long,
        headers: auditActorHeaders(opts),
        body: JSON.stringify(body),
      },
    ),
  listCollections: () => api<KnowledgeCollection[]>("/knowledge/collections"),
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
    api<KnowledgeCollection>(`/knowledge/collections/${seg(id)}`),
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
    api<KnowledgeCollection>(`/knowledge/collections/${seg(id)}`, {
      method: "PUT",
      headers: auditActorHeaders(options),
      body: JSON.stringify(body),
    }),
  deleteCollection: (id: string, options?: AuditOptions) =>
    api<void>(`/knowledge/collections/${seg(id)}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  listDocuments: (collectionId: string) =>
    api<KnowledgeDocument[]>(
      `/knowledge/collections/${seg(collectionId)}/documents`,
    ),
  /**
   * Uploads a document by streaming the browser's request body (the JSON with
   * `title`, `filename` and `content_base64`) straight on to the gateway, which
   * enforces the size limit and checks the content. Returns the gateway's
   * response to pass back as is.
   */
  uploadDocument: (
    collectionId: string,
    body: ReadableStream<Uint8Array>,
    contentType: string,
    options?: AuditOptions & { signal?: AbortSignal },
  ) =>
    apiStream(`/knowledge/collections/${seg(collectionId)}/documents`, {
      method: "POST",
      signal: options?.signal,
      timeoutMs: API_TIMEOUT_MS.long,
      headers: { "Content-Type": contentType, ...auditActorHeaders(options) },
      body,
      duplex: "half",
    }),
  deleteDocument: (id: string, options?: AuditOptions) =>
    api<void>(`/knowledge/documents/${seg(id)}`, {
      method: "DELETE",
      headers: auditActorHeaders(options),
    }),
  reindexCollection: (id: string, options?: AuditOptions) =>
    api<KnowledgeReindexResult>(`/knowledge/collections/${seg(id)}/reindex`, {
      method: "POST",
      timeoutMs: API_TIMEOUT_MS.long,
      headers: auditActorHeaders(options),
    }),
  reindexDocument: (id: string, options?: AuditOptions) =>
    api<KnowledgeDocument>(`/knowledge/documents/${seg(id)}/reindex`, {
      method: "POST",
      headers: auditActorHeaders(options),
    }),
  searchCollection: (id: string, body: { query: string; top_k?: number }) =>
    api<KnowledgeHit[]>(`/knowledge/collections/${seg(id)}/search`, {
      method: "POST",
      body: JSON.stringify(body),
    }),
  getModelCollections: (modelId: string) =>
    api<ModelKnowledgeCollections>(`/models/${seg(modelId)}/knowledge`),
  setModelCollections: (
    modelId: string,
    collection_ids: string[],
    options?: AuditOptions,
  ) =>
    api<ModelKnowledgeCollections>(`/models/${seg(modelId)}/knowledge`, {
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
