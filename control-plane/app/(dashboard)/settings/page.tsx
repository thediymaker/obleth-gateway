import { requireAdmin } from "@/lib/auth/roles";
import { SettingsTabs } from "@/components/settings-tabs";
import { VersionCard } from "@/components/version-card";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

const SETTINGS_TABS = ["alerts","routing","boons","compression","knowledge","energy","data","slurm","assistant","about"];

export default async function SettingsPage({ searchParams }: { searchParams: Promise<{ tab?: string }> }) {
  await requireAdmin();
  const { tab } = await searchParams;
  const [settings, autoRouter, boons, compressor, charo, energy, knowledge, models, retention, slurm, routerReadiness] =
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
    ]);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Settings</h1>
        <p className="text-sm text-muted-foreground">
          Configure alerting, routing, and data retention. Changes apply immediately to the running
          gateway&mdash;no restart required.
        </p>
      </div>
      <SettingsTabs
        alertSettings={settings}
        autoRouter={autoRouter}
        boons={boons}
        charo={charo}
        compressor={compressor}
        energy={energy}
        knowledge={knowledge}
        models={models}
        retention={retention}
        routerReadiness={routerReadiness}
        slurm={slurm}
        versionCard={<VersionCard />}
        initialTab={tab && SETTINGS_TABS.includes(tab) ? tab : undefined}
      />
    </div>
  );
}
