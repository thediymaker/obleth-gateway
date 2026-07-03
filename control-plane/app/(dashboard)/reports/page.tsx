import { ReportsDashboard } from "@/components/reports-dashboard";
import { obleth } from "@/lib/obleth";
import type { ApiKey, Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function ReportsPage() {
  const [tenants, keys] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.listKeys(), [] as ApiKey[]),
  ]);
  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Reports</h1>
        <p className="text-sm text-muted-foreground">
          Historical usage from the permanent daily rollup. Pick a date range and team, explore
          the charts, and export a CSV with exactly the columns you need.
        </p>
      </div>
      <ReportsDashboard tenants={tenants} keys={keys} />
    </div>
  );
}
