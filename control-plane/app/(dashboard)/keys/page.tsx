import { KeysList } from "@/components/access/keys-list";
import { EMPTY_KEY_FILTERS, keyListQuery, keyWindow, type KeyFilters, type KeySort } from "@/lib/access-model";
import { requireAdmin } from "@/lib/auth/roles";
import { loadKeyPage, type KeyPageData } from "@/lib/key-page-data";
import { obleth, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

const EMPTY_PAGE: KeyPageData = { keys: [], usage: [], budgets: [], next: null };

export default async function KeysPage({ searchParams }: { searchParams: Promise<{ tenant?: string; key?: string; new?: string }> }) {
  await requireAdmin();
  const params = await searchParams;
  // One window for every page of this visit, so a "Load more" continues the
  // same order.
  const window = keyWindow(Date.now());
  const filters: KeyFilters = { ...EMPTY_KEY_FILTERS, tenantId: params.tenant ?? "" };
  const sort: KeySort = "last-used";
  const [tenants, page, opened, stats, networks] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(loadKeyPage(keyListQuery(filters, sort, window)), EMPTY_PAGE),
    // A key linked to by address, which may not be on the first page.
    params.key ? safe(loadKeyPage({ ids: [params.key], sinceMs: window.sinceMs, untilMs: window.untilMs }), null) : null,
    safe(obleth.keyStats({ ...window, recentMs: window.untilMs - 86_400_000 }), null),
    safe(obleth.getNetworkSettings(), null),
  ]);

  return (
    <KeysList
      tenants={tenants}
      first={{ filters, sort, page }}
      usageWindow={window}
      stats={stats}
      opened={opened}
      initial={{ tenant: params.tenant, key: params.key, newFor: params.new }}
      named={networks?.networks ?? []}
    />
  );
}
