import { KeysList } from "@/components/access/keys-list";
import { requireAdmin } from "@/lib/auth/roles";
import { obleth, type ApiKey, type BudgetUsage, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

// Per-key use covers 30 days: long enough for "last used" to mean something
// for keys that aren't hit daily, while bounding the ClickHouse scan.
const KEY_USAGE_WINDOW_MS = 30 * 24 * 60 * 60 * 1000;

export default async function KeysPage({ searchParams }: { searchParams: Promise<{ tenant?: string; key?: string; new?: string }> }) {
  await requireAdmin();
  const params = await searchParams;
  const [tenants, keys, usage, budgets] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.listKeys(), [] as ApiKey[]),
    safe(obleth.keyUsageForDashboard({ sinceMs: Date.now() - KEY_USAGE_WINDOW_MS, limit: 5000 }), []),
    safe(obleth.budgetUsage(), [] as BudgetUsage[]),
  ]);

  return (
    <KeysList
      tenants={tenants}
      keys={keys}
      usage={usage}
      budgets={budgets}
      initial={{ tenant: params.tenant, key: params.key, newFor: params.new }}
    />
  );
}
