import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { CharoWorkspace } from "@/components/charo/workspace";

export const dynamic = "force-dynamic";

export default async function CharoWorkspacePage() {
  const settings = await safe(obleth.getCharoSettings(), null);
  const models = await safe(obleth.listModels(), []);
  // When Charo is explicitly disabled, the dashboard layout does not mount the
  // shared thread provider, so the context-dependent workspace would throw.
  // Render a standalone notice instead. A null settings fetch is a gateway
  // hiccup, not a deliberate disable — fail open and show the workspace.
  const disabled = settings !== null && !settings.enabled;
  return (
    <div className="space-y-4">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Charo</h1>
        <p className="text-sm text-muted-foreground">
          Chat, run tools, and review this session&apos;s benchmark runs. Runs are in-memory and clear on reload.
        </p>
      </div>
      {disabled ? (
        <p className="rounded-md border border-border bg-muted/30 px-4 py-6 text-sm text-muted-foreground">
          Charo is currently disabled. Enable it in Settings &rsaquo; Assistant to use the workspace.
        </p>
      ) : (
        <CharoWorkspace settings={settings} models={models} />
      )}
    </div>
  );
}
