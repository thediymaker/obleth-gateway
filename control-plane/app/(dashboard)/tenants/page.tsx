import { TenantTable } from "@/components/tenant-table";
import { obleth, type ModelRoute, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function TenantsPage() {
  const [tenants, models] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.listModels(), [] as ModelRoute[]),
  ]);
  const modelNames = models.map((m) => m.model_name);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Tenants</h1>
        <p className="text-sm text-muted-foreground">
          Fairshare units, safety limits, schedules, budgets, and model access.
        </p>
      </div>

      <TenantTable tenants={tenants} models={modelNames} />
    </div>
  );
}
