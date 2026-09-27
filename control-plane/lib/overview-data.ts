import { obleth, type AuditEntry, type CacheStats, type RouterReadinessView, type UsageModelAgg, type UsageTimePoint } from "@/lib/obleth";
import { EMPTY_OVERVIEW_SUMMARY, fetchOverviewSummary, type OverviewSummary } from "@/lib/overview-summary";
import { RANGE_BUCKET_MS, RANGE_MS, previousTotals, type OverviewRange } from "@/lib/overview-model";
import { safe } from "@/lib/safe";

export const TOP_TENANTS = 5;

export interface OverviewTenantRow {
  id: string;
  name: string;
  requests: number;
  tokens: number;
  /** Null when the gateway predates per-tenant cost. */
  cost: number | null;
}

/** Everything on the Overview that follows the range control. */
export interface OverviewWindow {
  range: OverviewRange;
  now: number;
  summary: OverviewSummary;
  /** The window before, for the "vs yesterday" comparisons. Null when the read failed. */
  previous: ReturnType<typeof previousTotals> | null;
  /** One read spanning the current and the previous window, split client-side. */
  series: UsageTimePoint[];
  models: UsageModelAgg[];
  tenants: OverviewTenantRow[];
  /** Everything past the top rows, summed, so the table still adds up. */
  otherTenants: { count: number; requests: number; tokens: number; cost: number | null };
  cache: CacheStats | null;
}

/** The slower-moving context around the numbers: recent changes and routing readiness. */
export interface OverviewContext {
  audit: AuditEntry[];
  readiness: RouterReadinessView | null;
  tenantNames: Record<string, string>;
}

export async function loadOverviewWindow(range: OverviewRange, now = Date.now()): Promise<OverviewWindow> {
  const since = now - RANGE_MS[range];
  const twoWindows = now - 2 * RANGE_MS[range];
  const [summary, both, series, models, usage, tenants, cache] = await Promise.all([
    safe(fetchOverviewSummary(since), EMPTY_OVERVIEW_SUMMARY),
    safe(fetchOverviewSummary(twoWindows).then((s) => s as OverviewSummary | null), null),
    safe(obleth.usageSeries(RANGE_BUCKET_MS[range], twoWindows), []),
    safe(obleth.usageByModel(since), []),
    safe(obleth.usage(since), []),
    safe(obleth.listTenants(), []),
    safe(obleth.cacheStats(since).then((c) => c as CacheStats | null), null),
  ]);
  const names = new Map(tenants.map((t) => [t.id, t.name]));
  const hasCost = usage.some((u) => u.cost_usd !== undefined);
  const rows = usage
    .map<OverviewTenantRow>((u) => ({
      id: u.tenant_id,
      name: names.get(u.tenant_id) ?? "Deleted tenant",
      requests: Number(u.requests),
      tokens: Number(u.total_tokens),
      cost: hasCost ? Number(u.cost_usd ?? 0) : null,
    }))
    .sort((a, b) => b.requests - a.requests);
  const rest = rows.slice(TOP_TENANTS);
  return {
    range,
    now,
    summary,
    previous: both ? previousTotals(summary, both) : null,
    series,
    models,
    tenants: rows.slice(0, TOP_TENANTS),
    otherTenants: {
      count: rest.length,
      requests: rest.reduce((n, r) => n + r.requests, 0),
      tokens: rest.reduce((n, r) => n + r.tokens, 0),
      cost: hasCost ? rest.reduce((n, r) => n + (r.cost ?? 0), 0) : null,
    },
    cache,
  };
}

export async function loadOverviewContext(): Promise<OverviewContext> {
  const [audit, readiness, tenants] = await Promise.all([
    safe(obleth.audit(8), []),
    safe(obleth.getRouterReadiness().then((r) => r as RouterReadinessView | null), null),
    safe(obleth.listTenants(), []),
  ]);
  return { audit, readiness, tenantNames: Object.fromEntries(tenants.map((t) => [t.id, t.name])) };
}
