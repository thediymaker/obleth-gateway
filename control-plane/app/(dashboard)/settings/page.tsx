import { SettingsTabs } from "@/components/settings-tabs";
import { VersionCard } from "@/components/version-card";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function SettingsPage() {
  const [settings, autoRouter, boons, compressor, charo, energy, models, retention, slurm] =
    await Promise.all([
      safe(obleth.getAlertSettings(), null),
      safe(obleth.getAutoRouterSettings(), null),
      safe(obleth.getBoonSettings(), null),
      safe(obleth.getCompressorStatus(), null),
      safe(obleth.getCharoSettings(), null),
      safe(obleth.getEnergySettings(), null),
      safe(obleth.listModels(), []),
      safe(obleth.getUsageRetention(), null),
      safe(obleth.getSlurmSettings(), null),
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
        models={models}
        retention={retention}
        slurm={slurm}
        versionCard={<VersionCard />}
      />
    </div>
  );
}
