import { notFound, redirect } from "next/navigation";
import { TenantPage } from "@/components/access/tenant-page";
import { tenantHref } from "@/lib/access-model";
import { requireAdmin } from "@/lib/auth/roles";
import { listUsers, type AdminUser } from "@/lib/auth/users";
import { accounts, type Member } from "@/lib/obleth-accounts";
import { obleth, type ApiKey, type BudgetUsage, type ModelRoute, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function TenantDetailPage({ params }: { params: Promise<{ name: string }> }) {
  await requireAdmin();
  const name = decodeURIComponent((await params).name);
  const tenants = await safe(obleth.listTenants(), [] as Tenant[]);
  const tenant = tenants.find((t) => t.name === name) ?? tenants.find((t) => t.id === name);
  if (!tenant) notFound();
  // An id leads to the name, so there is one address per tenant.
  if (tenant.name !== name) redirect(tenantHref(tenant));

  const [keys, users, members, budgets, models] = await Promise.all([
    safe(obleth.listKeys(tenant.id), [] as ApiKey[]),
    safe(listUsers(), [] as AdminUser[]),
    safe(accounts.listMembers(tenant.id), [] as Member[]),
    safe(obleth.budgetUsage(), [] as BudgetUsage[]),
    safe(obleth.listModels(), [] as ModelRoute[]),
  ]);

  return (
    <TenantPage
      key={tenant.id}
      tenant={tenant}
      tenants={tenants}
      keys={keys}
      members={members}
      users={[...new Set(users.map((u) => u.email.toLowerCase()))].sort()}
      budgets={budgets}
      models={models.map((m) => m.model_name).sort()}
    />
  );
}
