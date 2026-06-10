import {
  AlertSettingsForm,
  AutoRouterSettingsForm,
  BoonsSettingsForm,
  UsageRetentionForm,
} from "@/components/settings-form";
import { BackupRestore } from "@/components/backup-restore";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function SettingsPage() {
  const settings = await safe(obleth.getAlertSettings(), null);
  const autoRouter = await safe(obleth.getAutoRouterSettings(), null);
  const boons = await safe(obleth.getBoonSettings(), null);
  const models = await safe(obleth.listModels(), []);
  const retention = await safe(obleth.getUsageRetention(), null);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Settings</h1>
        <p className="text-sm text-muted-foreground">
          Configure operational alerting. Changes apply immediately to the running gateway&mdash;no
          restart required.
        </p>
      </div>
      <AlertSettingsForm settings={settings} />
      <AutoRouterSettingsForm settings={autoRouter} models={models} />
      <BoonsSettingsForm settings={boons} models={models} />
      <UsageRetentionForm retention={retention} />
      <BackupRestore />
    </div>
  );
}
