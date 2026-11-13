import { TenantsList } from "@/components/access/tenants-list";
import { requireAdmin } from "@/lib/auth/roles";
import { listUsers, type AdminUser } from "@/lib/auth/users";
import { accounts, type MemberCount } from "@/lib/obleth-accounts";
import { obleth, type ApiKey, type BudgetUsage, type FairshareGroup, type ModelRoute, type Tenant, type UsageAgg, type UsageDailyRow } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

const DAY_MS = 86_400_000;

export default async function TenantsPage() {
  await requireAdmin();
  const now = new Date();
  const monthStart = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1)).toISOString().slice(0, 10);
  const [tenants, keys, users, members, usage24h, budgets, month, models, groups] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.listKeys(), [] as ApiKey[]),
    safe(listUsers(), [] as AdminUser[]),
    safe(accounts.memberCounts(), [] as MemberCount[]),
    safe(obleth.usage(now.getTime() - DAY_MS), [] as UsageAgg[]),
    safe(obleth.budgetUsage(), [] as BudgetUsage[]),
    safe(obleth.usageDaily({ startDay: monthStart, endDay: now.toISOString().slice(0, 10), groupBy: "tenant" }), [] as UsageDailyRow[]),
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe(obleth.listFairshareGroups(), [] as FairshareGroup[]),
  ]);
  const monthSpend: Record<string, number> = {};
  for (const row of month) monthSpend[row.tenant_id] = (monthSpend[row.tenant_id] ?? 0) + Number(row.cost_usd);

  return (
    <TenantsList
      tenants={tenants}
      keys={keys}
      users={users}
      memberCounts={Object.fromEntries(members.map((m) => [m.tenant_id, m.members]))}
      usage24h={usage24h}
      budgets={budgets}
      monthSpend={monthSpend}
      models={models.map((m) => m.model_name)}
      fairshareGroups={groups.map((g) => g.name).sort()}
    />
  );
}
