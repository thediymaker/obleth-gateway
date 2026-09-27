import { requireAdmin } from "@/lib/auth/roles";
import { FairshareDashboard } from "@/components/fairshare-dashboard";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function FairsharePage({ searchParams }: { searchParams: Promise<{ pool?: string }> }) {
  await requireAdmin();
  const { pool } = await searchParams;
  // Only tenant metadata is loaded here (bounded - hundreds, not the full key
  // fleet). Key/usage data is fetched client-side via top-N limited endpoints
  // so the page stays fast even with 100k+ keys.
  const tenants = await safe(obleth.listTenants(), []);
  const tenantNames = Object.fromEntries(tenants.map((t) => [t.id, t.name]));

  return <FairshareDashboard tenantNames={tenantNames} initialPool={pool?.trim() || "all"} />;
}
