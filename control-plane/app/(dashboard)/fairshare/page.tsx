import { CapacityControl } from "@/components/capacity-control";
import { FairshareDashboard } from "@/components/fairshare-dashboard";
import { Card, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function FairsharePage() {
  // Only tenant metadata is loaded here (bounded — hundreds, not the full key
  // fleet). Key/usage data is fetched client-side via top-N limited endpoints
  // so the page stays fast even with 100k+ keys.
  const [tenants, capacity] = await Promise.all([
    safe(obleth.listTenants(), []),
    safe<{ max_in_flight: number } | null>(obleth.getCapacity(), null),
  ]);

  const tenantNames = Object.fromEntries(tenants.map((t) => [t.id, t.name]));
  const tenantGroups = Object.fromEntries(tenants.map((t) => [t.id, t.fairshare_group]));

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Fairshare</h1>
        <p className="text-sm text-muted-foreground max-w-2xl">
          Live scheduler state — slot allocation, group pools, and throughput. Read-only polls against the admin API;
          does not affect the data plane hot path.
        </p>
      </div>
      {capacity && (
        <Card>
          <CardHeader className="gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div>
              <CardTitle>Gateway capacity</CardTitle>
              <CardDescription>Maximum concurrent requests admitted across all tenants</CardDescription>
            </div>
            <div>
              <CapacityControl initial={capacity.max_in_flight} />
            </div>
          </CardHeader>
        </Card>
      )}
      <FairshareDashboard tenantNames={tenantNames} tenantGroups={tenantGroups} />
    </div>
  );
}
