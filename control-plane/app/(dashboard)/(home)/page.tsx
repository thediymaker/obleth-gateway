import { requireAdmin } from "@/lib/auth/roles";
import { OverviewDashboard } from "@/components/overview-dashboard";
import { loadOverviewContext, loadOverviewWindow } from "@/lib/overview-data";
import { obleth, type FairshareLiveView, type LiveStats, type ModelHealthSummary } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function OverviewPage() {
  await requireAdmin();
  const [overviewWindow, context, models, health, fairshare, stats] = await Promise.all([
    loadOverviewWindow("24h"),
    loadOverviewContext(),
    safe(obleth.listModels(), []),
    safe<ModelHealthSummary[]>(obleth.modelHealth(), []),
    safe<FairshareLiveView | undefined>(obleth.fairshareLive(), undefined),
    safe<LiveStats | undefined>(obleth.stats(), undefined),
  ]);

  return (
    <OverviewDashboard
      initialModels={models}
      initialWindow={overviewWindow}
      initialContext={context}
      initialHealth={health}
      initialFairshare={fairshare}
      initialStats={stats}
    />
  );
}
