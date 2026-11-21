import { KeysList } from "@/components/access/keys-list";
import { KEY_USAGE_LIMIT, KEY_USAGE_WINDOW_MS } from "@/lib/access-model";
import { requireAdmin } from "@/lib/auth/roles";
import { obleth, type ApiKey, type BudgetUsage, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function KeysPage({ searchParams }: { searchParams: Promise<{ tenant?: string; key?: string; new?: string }> }) {
  await requireAdmin();
  const params = await searchParams;
  const [tenants, keys, usage, budgets, networks] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.listKeys(), [] as ApiKey[]),
    safe(obleth.usageKeysSummary({ sinceMs: Date.now() - KEY_USAGE_WINDOW_MS, limit: KEY_USAGE_LIMIT }), []),
    safe(obleth.budgetUsage(), [] as BudgetUsage[]),
    safe(obleth.getNetworkSettings(), null),
  ]);

  return (
    <KeysList
      tenants={tenants}
      keys={keys}
      usage={usage}
      budgets={budgets}
      initial={{ tenant: params.tenant, key: params.key, newFor: params.new }}
      named={networks?.networks ?? []}
    />
  );
}
