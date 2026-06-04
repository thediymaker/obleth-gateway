import { AlertSettingsForm } from "@/components/settings-form";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function SettingsPage() {
  const settings = await safe(obleth.getAlertSettings(), null);

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
    </div>
  );
}
