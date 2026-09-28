import type { TenantOverviewData } from "@/lib/access-model";
import { fillDays } from "@/lib/access-model";
import { obleth, type UsageAgg, type UsageDailyRow } from "@/lib/obleth";
import { safe } from "@/lib/safe";

const DAY_MS = 86_400_000;

function isoDay(ms: number) {
  return new Date(ms).toISOString().slice(0, 10);
}

/** A tenant page's traffic, spend, models, keys and recent changes, in one read. */
export async function loadTenantOverview(id: string, now = Date.now()): Promise<TenantOverviewData> {
  const start = isoDay(now - 29 * DAY_MS);
  const end = isoDay(now);
  const [today, both, histogram, facets, days, models, keys, audit] = await Promise.all([
    safe(obleth.usage(now - DAY_MS), [] as UsageAgg[]),
    safe(obleth.usage(now - 2 * DAY_MS).then((u) => u as UsageAgg[] | null), null),
    safe(obleth.usageLogHistogram({ tenantId: id, sinceMs: now - DAY_MS, bucketMs: 3_600_000 }), null),
    safe(obleth.usageLogFacets({ tenantId: id, status: "error", sinceMs: now - DAY_MS }), null),
    safe(obleth.usageDaily({ startDay: start, endDay: end, groupBy: "day", tenantId: id }), [] as UsageDailyRow[]),
    safe(obleth.usageDaily({ startDay: start, endDay: end, groupBy: "model", tenantId: id }), [] as UsageDailyRow[]),
    safe(obleth.usageKeysSummary({ tenantId: id, sinceMs: now - 30 * DAY_MS, limit: 500 }), []),
    safe(obleth.audit(500), []),
  ]);
  const mine = today.find((u) => u.tenant_id === id);
  const total = both?.find((u) => u.tenant_id === id);
  const requests24h = Number(mine?.requests ?? 0);
  return {
    now,
    requests24h,
    previous24h: both ? Math.max(0, Number(total?.requests ?? 0) - requests24h) : null,
    failed24h: (histogram?.buckets ?? []).reduce((n, b) => n + Number(b.errors), 0),
    topFailure: facets?.failures?.[0] ?? null,
    days: fillDays(
      days.map((d) => ({ day: d.day.slice(0, 10), requests: Number(d.requests), failed: Number(d.error_requests), cost: Number(d.cost_usd), tokens: Number(d.total_tokens) })),
      start,
      end,
    ),
    models: models
      .map((m) => ({ model: m.model, requests: Number(m.requests), tokens: Number(m.total_tokens), cost: Number(m.cost_usd) }))
      .sort((a, b) => b.requests - a.requests),
    tokens30d: days.reduce((n, d) => n + Number(d.total_tokens), 0),
    inputTokens30d: days.reduce((n, d) => n + Number(d.input_tokens), 0),
    keys,
    audit: audit
      .filter((e) => e.entity_type === "tenant" && e.entity_id === id)
      .slice(0, 10)
      .map((e) => ({ ts: e.ts, actor: e.actor, action: e.action, detail: e.detail })),
  };
}
