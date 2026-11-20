import { requireAdmin } from "@/lib/auth/roles";
import { KeyAccessView } from "@/components/access/key-access-view";
import { ReportsDashboard } from "@/components/reports/reports-dashboard";
import { isKeyAccessRange, keyAccessRange, type KeyAccessRange } from "@/lib/key-access-model";
import { KEY_REJECTION_ROWS, obleth } from "@/lib/obleth";
import type { KeyRejectionAgg, KeySource, ModelRoute, Tenant } from "@/lib/obleth";
import { KEY_OPTION_FIELDS, keyOptions, tenantOptions } from "@/lib/logs-model";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

type Params = { tab?: string; range?: string; tenant?: string; q?: string };

export default async function ReportsPage({ searchParams }: { searchParams: Promise<Params> }) {
  await requireAdmin();
  const q = await searchParams;
  if (q.tab === "key-access") return <KeyAccessTab q={q} />;
  const [tenants, keys, models, retention] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.listKeyFields(KEY_OPTION_FIELDS), []),
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe(obleth.getUsageRetention(), null),
  ]);
  return (
    <ReportsDashboard
      tenants={tenantOptions(tenants)}
      keys={keyOptions(keys)}
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
    safe(obleth.keyRejections({ sinceMs, tenantId, limit: KEY_REJECTION_ROWS }).then((r) => r as KeyRejectionAgg[] | null), null),
  ]);
  return (
    <KeyAccessView
      key={`${range}:${tenantId ?? ""}`}
      tenants={tenantOptions(tenants)}
      initialRange={range}
      initialTenant={tenantId ?? ""}
      initialSearch={q.q ?? ""}
      initial={sources && rejections ? { sinceMs, sources, rejections } : null}
    />
  );
}
