import { redirect } from "next/navigation";
import { requireAdmin } from "@/lib/auth/roles";
import { SettingsPage } from "@/components/settings/settings-page";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { TAB_ANCHOR } from "@/lib/settings-model";
import { CONTROL_PLANE_SHA, CONTROL_PLANE_VERSION, fetchLatestRelease, isNewer } from "@/lib/version";

export const dynamic = "force-dynamic";

export default async function Page({ searchParams }: { searchParams: Promise<{ tab?: string }> }) {
  await requireAdmin();
  const { tab } = await searchParams;
  // Old links: Slurm moved to Deployments, retrieval to Knowledge.
  if (tab === "slurm") redirect("/deployments?slurm=1");
  if (tab === "knowledge") redirect("/knowledge?tab=retrieval");
  const [alerts, router, boons, compressor, charo, energy, knowledge, models, retention, slurm, readiness, gateway, latest] =
    await Promise.all([
      safe(obleth.getAlertSettings(), null),
      safe(obleth.getAutoRouterSettings(), null),
      safe(obleth.getBoonSettings(), null),
      safe(obleth.getCompressorStatus(), null),
      safe(obleth.getCharoSettings(), null),
      safe(obleth.getEnergySettings(), null),
      safe(obleth.getKnowledgeSettings(), null),
      safe(obleth.listModels(), []),
      safe(obleth.getUsageRetention(), null),
      safe(obleth.getSlurmSettings(), null),
      safe(obleth.getRouterReadiness(), null),
      safe(obleth.gatewayVersion(), null),
      fetchLatestRelease(),
    ]);

  return (
    <SettingsPage
      anchor={tab ? TAB_ANCHOR[tab] : undefined}
      data={{
        alerts, router, readiness, boons, compressor, knowledge, energy, charo, retention, slurm, models,
        version: {
          gateway,
          controlPlane: { version: CONTROL_PLANE_VERSION, sha: CONTROL_PLANE_SHA },
          latest: latest?.tag ?? null,
          updateAvailable: latest !== null && isNewer(latest.tag, CONTROL_PLANE_VERSION),
        },
      }}
    />
  );
}
