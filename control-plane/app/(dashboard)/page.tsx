import Link from "next/link";
import { OverviewDashboard } from "@/components/overview-dashboard";
import { computeOverviewSummary } from "@/lib/overview-summary";
import { obleth, type CacheStats, type FairshareLiveView, type LiveStats, type ModelHealthSummary } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

const DAY_MS = 86_400_000;
const HOUR_MS = 3_600_000;

export default async function OverviewPage() {
  const now = Date.now();
  const dayAgo = now - DAY_MS;
  const hourAgo = now - HOUR_MS;

  const [
    tenants,
    keys,
    models,
    usage,
    usageByModel,
    usageByKey,
    costs,
    volumeSeries,
    tenantSeries,
    audit,
    cacheStats,
    health,
    fairshare,
    stats,
  ] = await Promise.all([
    safe(obleth.listTenants(), []),
    safe(obleth.listKeys(), []),
    safe(obleth.listModels(), []),
    safe(obleth.usage(dayAgo), []),
    safe(obleth.usageByModel(hourAgo), []),
    safe(obleth.usageByKey(hourAgo, 10), []),
    safe(obleth.costs(dayAgo), []),
    safe(obleth.usageSeries(300_000, dayAgo), []),
    safe(obleth.usageSeriesByTenant(60_000, hourAgo), []),
    safe(obleth.audit(8), []),
    safe<CacheStats | undefined>(obleth.cacheStats(dayAgo), undefined),
    safe<ModelHealthSummary[]>(obleth.modelHealth(), []),
    safe<FairshareLiveView | undefined>(obleth.fairshareLive(), undefined),
    safe<LiveStats | undefined>(obleth.stats(), undefined),
  ]);

  const summary = computeOverviewSummary(tenants, keys, models, usage, costs);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Overview</h1>
        <p className="text-sm text-muted-foreground">
          Gateway traffic, capacity, and route health. For full scheduler state see{" "}
          <Link href="/fairshare" className="underline underline-offset-2 hover:text-foreground">
            Fairshare
          </Link>
        </p>
      </div>

      <OverviewDashboard
        tenants={tenants}
        models={models}
        initialSummary={summary}
        initialVolumeSeries={volumeSeries}
        initialTenantUsage={usage}
        initialTenantSeries={tenantSeries}
        initialModelUsage={usageByModel}
        initialKeyUsage={usageByKey}
        initialAudit={audit}
        initialCacheStats={cacheStats}
        initialHealth={health}
        initialFairshare={fairshare}
        initialStats={stats}
      />
    </div>
  );
}
