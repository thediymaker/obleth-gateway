import { requireAdmin } from "@/lib/auth/roles";
import { KeyAccessView } from "@/components/access/key-access-view";
import { ReportsDashboard } from "@/components/reports/reports-dashboard";
import { isKeyAccessRange, keyAccessRange, type KeyAccessRange } from "@/lib/key-access-model";
import { obleth } from "@/lib/obleth";
import type { ApiKey, KeyRejectionAgg, KeySource, ModelRoute, Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

type Params = { tab?: string; range?: string; tenant?: string; q?: string };

export default async function ReportsPage({ searchParams }: { searchParams: Promise<Params> }) {
  await requireAdmin();
  const q = await searchParams;
  if (q.tab === "key-access") return <KeyAccessTab q={q} />;
  const [tenants, keys, models, retention] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.listKeys(), [] as ApiKey[]),
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe(obleth.getUsageRetention(), null),
  ]);
  return (
    <ReportsDashboard
      tenants={[...tenants].sort((a, b) => a.name.localeCompare(b.name))}
      keys={keys}
      models={models.map((m) => m.model_name).sort((a, b) => a.localeCompare(b))}
      retentionDays={retention?.days ?? null}
      modelTypes={Object.fromEntries(models.map((m) => [m.model_name, m.model_type]))}
    />
  );
}

/** Where each key was used from, and the requests that were refused. */
async function KeyAccessTab({ q }: { q: Params }) {
  const range: KeyAccessRange = isKeyAccessRange(q.range) ? q.range : "24h";
  const tenantId = q.tenant || undefined;
  const sinceMs = Date.now() - keyAccessRange(range).ms;
  const [tenants, sources, rejections] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.keySources({ sinceMs, tenantId }).then((r) => r as KeySource[] | null), null),
    safe(obleth.keyRejections({ sinceMs, tenantId }).then((r) => r as KeyRejectionAgg[] | null), null),
  ]);
  return (
    <KeyAccessView
      key={`${range}:${tenantId ?? ""}`}
      tenants={[...tenants].sort((a, b) => a.name.localeCompare(b.name)).map((t) => ({ id: t.id, name: t.name }))}
      initialRange={range}
      initialTenant={tenantId ?? ""}
      initialSearch={q.q ?? ""}
      initial={sources && rejections ? { sinceMs, sources, rejections } : null}
    />
  );
}
