import { requireTenant } from "@/lib/auth/roles";
import { obleth } from "@/lib/obleth";
import { PortalKeys } from "@/components/portal/portal-keys";

export const dynamic = "force-dynamic";

export default async function PortalKeysPage() {
  const tenantId = await requireTenant();
  const keys = await obleth.listKeys(tenantId);
  return <PortalKeys keys={keys} />;
}
