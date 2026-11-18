import { MODEL_SERIES_BUCKET_MS, type ModelOverviewData, type ModelTenantRow } from "@/lib/models-model";
import { type AuditEntry, obleth, type UsageModelAgg } from "@/lib/obleth";
import { safe } from "@/lib/safe";

const DAY_MS = 86_400_000;

export async function loadModelOverview(id: string, name: string, now = Date.now()): Promise<ModelOverviewData> {
  const day = now - DAY_MS;
  const twoDays = now - 2 * DAY_MS;
  // Each read is scoped to this model: the gateway filters before it groups,
  // and the audit log answers from its entity index, so the page costs the
  // same whatever the fleet's size or how long ago the model last changed.
  const [today, series, breakdown, audit] = await Promise.all([
    safe(obleth.usageByModel({ sinceMs: day, model: name }), [] as UsageModelAgg[]),
    safe(obleth.usageSeriesByModel(name, MODEL_SERIES_BUCKET_MS, twoDays).then((s) => s as typeof s | null), null),
    safe(obleth.usageBreakdownByModel(name, day, 200), []),
    safe(obleth.auditQuery({ entityType: "model", entityId: id, limit: 8 }), [] as AuditEntry[]),
  ]);
  const usage = today[0] ?? null;
  // The day before, from the two-day series the chart already shows.
  const previousRequests = series ? series.filter((p) => p.bucket_ms < day).reduce((n, p) => n + Number(p.requests), 0) : null;
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
    previousRequests,
    series: series ?? [],
    tenants: [...byTenant.values()].sort((a, b) => b.requests - a.requests).slice(0, 5),
    audit,
  };
}
