import { FairshareDashboard } from "@/components/fairshare-dashboard";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function FairsharePage() {
  // Only tenant metadata is loaded here (bounded - hundreds, not the full key
  // fleet). Key/usage data is fetched client-side via top-N limited endpoints
  // so the page stays fast even with 100k+ keys.
  const tenants = await safe(obleth.listTenants(), []);

  const tenantNames = Object.fromEntries(tenants.map((t) => [t.id, t.name]));
  const tenantGroups = Object.fromEntries(tenants.map((t) => [t.id, t.fairshare_group]));

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Fairshare</h1>
        <p className="max-w-2xl text-sm text-muted-foreground">
          Live scheduler state for group pools, tenant contention, and the workload driving it.
        </p>
      </div>
      <FairshareDashboard tenantNames={tenantNames} tenantGroups={tenantGroups} />
    </div>
  );
}
