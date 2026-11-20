import { PortalUsage } from "@/components/portal/portal-usage";
import { requireTenant } from "@/lib/auth/roles";
import { obleth, type KeyUsageSummary, type UsageAgg, type UsageLogEntry } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function PortalUsagePage() {
  const tenantId = await requireTenant();
  const sinceMs = Date.now() - 24 * 60 * 60_000;
  const [usage, keyUsage, recent] = await Promise.all([
    safe(obleth.usage({ tenantId }), [] as UsageAgg[]),
    safe(obleth.usageKeysSummary({ tenantId, sinceMs, limit: 100 }), [] as KeyUsageSummary[]),
    safe(obleth.usageLogs({ tenantId, sinceMs, limit: 50 }), [] as UsageLogEntry[]),
  ]);
  return (
    <PortalUsage
      usage={usage}
      keyUsage={keyUsage}
      recent={recent}
      gatewayBase={process.env.OBLETH_PROXY_BASE_URL ?? "http://localhost:8080"}
    />
  );
}
