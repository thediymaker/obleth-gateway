import { MODEL_SERIES_BUCKET_MS, type ModelOverviewData, type ModelTenantRow } from "@/lib/models-model";
import { obleth, type UsageModelAgg } from "@/lib/obleth";
import { safe } from "@/lib/safe";

const DAY_MS = 86_400_000;

export async function loadModelOverview(id: string, name: string, now = Date.now()): Promise<ModelOverviewData> {
  const day = now - DAY_MS;
  const twoDays = now - 2 * DAY_MS;
  const [today, both, series, breakdown, audit] = await Promise.all([
    safe(obleth.usageByModel(day), [] as UsageModelAgg[]),
    safe(obleth.usageByModel(twoDays).then((u) => u as UsageModelAgg[] | null), null),
    safe(obleth.usageSeriesByModel(name, MODEL_SERIES_BUCKET_MS, twoDays), []),
    safe(obleth.usageBreakdownByModel(name, day, 200), []),
    safe(obleth.audit(500), []),
  ]);
  const usage = today.find((u) => u.model === name) ?? null;
  const total = both?.find((u) => u.model === name);
  const byTenant = new Map<string, ModelTenantRow>();
  for (const row of breakdown) {
    const key = row.tenant_name || row.tenant_id;
    const t = byTenant.get(key) ?? { name: row.tenant_name || "Deleted tenant", requests: 0, tokens: 0 };
    t.requests += Number(row.requests);
    t.tokens += Number(row.total_tokens);
    byTenant.set(key, t);
  }
  return {
    now,
    usage,
    previousRequests: both ? Math.max(0, Number(total?.requests ?? 0) - Number(usage?.requests ?? 0)) : null,
    series,
    tenants: [...byTenant.values()].sort((a, b) => b.requests - a.requests).slice(0, 5),
    audit: audit.filter((e) => e.entity_type === "model" && e.entity_id === id).slice(0, 8),
  };
}
