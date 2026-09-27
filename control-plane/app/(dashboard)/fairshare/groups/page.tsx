import { requireAdmin } from "@/lib/auth/roles";
import { FairshareGroups } from "@/components/fairshare/groups";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function FairshareGroupsPage() {
  await requireAdmin();
  const [groups, tenants] = await Promise.all([safe(obleth.listFairshareGroups(), []), safe(obleth.listTenants(), [])]);
  return <FairshareGroups groups={groups} tenants={tenants} />;
}
