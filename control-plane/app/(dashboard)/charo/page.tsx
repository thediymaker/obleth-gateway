import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { CharoWorkspace } from "@/components/charo/workspace";

export const dynamic = "force-dynamic";

export default async function CharoWorkspacePage() {
  const settings = await safe(obleth.getCharoSettings(), null);
  const models = await safe(obleth.listModels(), []);
  return (
    <div className="space-y-4">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Charo</h1>
        <p className="text-sm text-muted-foreground">
          Chat, run tools, and review this session&apos;s benchmark runs. Runs are in-memory and clear on reload.
        </p>
      </div>
      <CharoWorkspace settings={settings} models={models} />
    </div>
  );
}
