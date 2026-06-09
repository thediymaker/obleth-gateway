import { RequestLogs } from "@/components/request-logs";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function LogsPage() {
  // Filter option metadata is loaded server-side (tenants are bounded; models
  // are a small registry). The log rows themselves stream in client-side from
  // the live, paginated feed so the page stays responsive under heavy volume.
  const [tenants, models] = await Promise.all([
    safe(obleth.listTenants(), []),
    safe(obleth.listModels(), []),
  ]);

  const tenantOptions = tenants
    .map((t) => ({ id: t.id, name: t.name }))
    .sort((a, b) => a.name.localeCompare(b.name));
  const modelOptions = models.map((m) => m.model_name).sort((a, b) => a.localeCompare(b));

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Request Logs</h1>
        <p className="text-sm text-muted-foreground max-w-2xl">
          Every request as it lands, newest first. Flip on Live Tail to watch traffic roll through,
          or pause it to filter and page back through history.
        </p>
      </div>
      <RequestLogs tenants={tenantOptions} models={modelOptions} />
    </div>
  );
}
