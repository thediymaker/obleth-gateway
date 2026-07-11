import { OverviewDashboard } from "@/components/overview-dashboard";
import { EMPTY_OVERVIEW_SUMMARY, fetchOverviewSummary } from "@/lib/overview-summary";
import { obleth, type FairshareLiveView, type LiveStats, type ModelHealthSummary } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

const DAY_MS = 86_400_000;
const HOUR_MS = 3_600_000;

export default async function OverviewPage() {
  const now = Date.now();
  const dayAgo = now - DAY_MS;
  const hourAgo = now - HOUR_MS;

  const [
    summary,
    models,
    usageByModel,
    volumeSeries,
    tenantSeries,
    health,
    fairshare,
    stats,
  ] = await Promise.all([
    safe(fetchOverviewSummary(), EMPTY_OVERVIEW_SUMMARY),
    safe(obleth.listModels(), []),
    safe(obleth.usageByModel(hourAgo), []),
    safe(obleth.usageSeries(300_000, dayAgo), []),
    safe(obleth.usageSeriesByTenant(60_000, hourAgo), []),
    safe<ModelHealthSummary[]>(obleth.modelHealth(), []),
    safe<FairshareLiveView | undefined>(obleth.fairshareLive(), undefined),
    safe<LiveStats | undefined>(obleth.stats(), undefined),
  ]);

  return (
    <div>
      <OverviewDashboard
        models={models}
        initialSummary={summary}
        initialVolumeSeries={volumeSeries}
        initialTenantSeries={tenantSeries}
        initialModelUsage={usageByModel}
        initialHealth={health}
        initialFairshare={fairshare}
        initialStats={stats}
      />
    </div>
  );
}
