import { requireAdmin } from "@/lib/auth/roles";
import { ReportsDashboard } from "@/components/reports/reports-dashboard";
import { obleth } from "@/lib/obleth";
import type { ApiKey, ModelRoute, Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function ReportsPage() {
  await requireAdmin();
  const [tenants, keys, models] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.listKeys(), [] as ApiKey[]),
    safe(obleth.listModels(), [] as ModelRoute[]),
  ]);
  return (
    <ReportsDashboard
      tenants={[...tenants].sort((a, b) => a.name.localeCompare(b.name))}
      keys={keys}
      models={models.map((m) => m.model_name).sort((a, b) => a.localeCompare(b))}
    />
  );
}
