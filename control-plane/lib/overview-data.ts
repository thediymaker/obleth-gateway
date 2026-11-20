import { obleth, type AuditEntry, type CacheStats, type OverviewView, type RouterReadinessView, type UsageModelAgg, type UsageTimePoint } from "@/lib/obleth";
import { EMPTY_OVERVIEW_SUMMARY, toOverviewSummary, type OverviewSummary } from "@/lib/overview-summary";
import { RANGE_BUCKET_MS, RANGE_MS, type OverviewRange } from "@/lib/overview-model";
import { safe } from "@/lib/safe";

export const TOP_TENANTS = 5;

export interface OverviewTenantRow {
  id: string;
  name: string;
  requests: number;
  tokens: number;
  cost: number;
}

/** Everything on the Overview that follows the range control. */
export interface OverviewWindow {
  range: OverviewRange;
  now: number;
  summary: OverviewSummary;
  /** The window before, for the "vs yesterday" comparisons. Null when the read failed. */
  previous: OverviewSummary | null;
  /** Buckets over the previous window and this one, split client-side. */
  series: UsageTimePoint[];
  models: UsageModelAgg[];
  tenants: OverviewTenantRow[];
  /** Everything past the top rows, summed, so the table still adds up. */
  otherTenants: { count: number; requests: number; tokens: number; cost: number };
  cache: CacheStats | null;
}

/** The slower-moving context around the numbers: recent changes and routing readiness. */
export interface OverviewContext {
  audit: AuditEntry[];
  readiness: RouterReadinessView | null;
  tenantNames: Record<string, string>;
}

export async function loadOverviewWindow(range: OverviewRange, now = Date.now()): Promise<OverviewWindow> {
  const view = await safe(
    obleth.overview(now - RANGE_MS[range], RANGE_BUCKET_MS[range]).then((v) => v as OverviewView | null),
    null,
  );
  if (!view) {
    return {
      range,
      now,
      summary: EMPTY_OVERVIEW_SUMMARY,
      previous: null,
      series: [],
      models: [],
      tenants: [],
      otherTenants: { count: 0, requests: 0, tokens: 0, cost: 0 },
      cache: null,
    };
  }
  const rows = view.tenants.map<OverviewTenantRow>((t) => ({
    id: t.tenant_id,
    name: t.name ?? "Deleted tenant",
    requests: Number(t.requests),
    tokens: Number(t.total_tokens),
    cost: Number(t.cost_usd),
  }));
  const rest = rows.slice(TOP_TENANTS);
  return {
    range,
    now,
    summary: toOverviewSummary(view.summary),
    previous: toOverviewSummary(view.previous),
    series: view.series,
    models: view.models,
    tenants: rows.slice(0, TOP_TENANTS),
    otherTenants: {
      count: rest.length,
      requests: rest.reduce((n, r) => n + r.requests, 0),
      tokens: rest.reduce((n, r) => n + r.tokens, 0),
      cost: rest.reduce((n, r) => n + r.cost, 0),
    },
    cache: view.cache,
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
