import { SettingsTabs } from "@/components/settings-tabs";
import { VersionCard } from "@/components/version-card";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function SettingsPage() {
  const settings = await safe(obleth.getAlertSettings(), null);
  const autoRouter = await safe(obleth.getAutoRouterSettings(), null);
  const boons = await safe(obleth.getBoonSettings(), null);
  const compressor = await safe(obleth.getCompressorStatus(), null);
  const charo = await safe(obleth.getCharoSettings(), null);
  const models = await safe(obleth.listModels(), []);
  const retention = await safe(obleth.getUsageRetention(), null);
  const slurm = await safe(obleth.getSlurmSettings(), null);

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
        models={models}
        retention={retention}
        slurm={slurm}
        versionCard={<VersionCard />}
      />
    </div>
  );
}
