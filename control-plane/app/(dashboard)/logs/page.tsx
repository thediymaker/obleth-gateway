import { requireAdmin } from "@/lib/auth/roles";
import { RequestLogs } from "@/components/logs/request-logs";
import { LOG_WINDOWS, type LogFilters } from "@/lib/logs-model";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

type Params = { requestId?: string; model?: string; status?: string; team?: string; key?: string; session?: string; window?: string };

export default async function LogsPage({ searchParams }: { searchParams: Promise<Params> }) {
  await requireAdmin();
  const q = await searchParams;
  // Filter options load here (tenants are bounded, models are a small
  // registry, keys are names and prefixes only); the rows stream in
  // client-side from the live, paginated feed.
  const [tenants, models, keys] = await Promise.all([
    safe(obleth.listTenants(), []),
    safe(obleth.listModels(), []),
    safe(obleth.listKeys(), []),
  ]);
  const tenantOptions = tenants.map((t) => ({ id: t.id, name: t.name })).sort((a, b) => a.name.localeCompare(b.name));
  const keyOptions = keys
    .map((k) => ({ id: k.id, name: k.name, prefix: k.key_prefix, tenantId: k.tenant_id }))
    .sort((a, b) => (a.name || a.prefix).localeCompare(b.name || b.prefix));
  const modelOptions = models.map((m) => m.model_name).sort((a, b) => a.localeCompare(b));

  // Other pages link here with a filter already chosen: `?status=error`,
  // `?model=<name>`, `?team=<tenant id>`, `?key=<key id>`, `?session=<id>`.
  const initial: Partial<LogFilters> = {
    ...(q.status === "error" || q.status === "success" ? { status: q.status } : {}),
    ...(q.model && modelOptions.includes(q.model) ? { model: q.model } : {}),
    ...(q.team && tenantOptions.some((t) => t.id === q.team) ? { tenantId: q.team } : {}),
    ...(q.key ? (() => { const k = keyOptions.find((x) => x.id === q.key); return k ? { keyId: k.id, tenantId: k.tenantId } : {}; })() : {}),
    ...(q.session ? { sessionId: q.session } : {}),
    ...(LOG_WINDOWS.some((w) => w.id === q.window) ? { window: q.window as LogFilters["window"] } : {}),
  };

  return (
    <RequestLogs
      key={q.requestId ?? JSON.stringify(initial)}
      tenants={tenantOptions}
      models={modelOptions}
      keys={keyOptions}
      initial={initial}
      initialRequestId={q.requestId}
    />
  );
}
