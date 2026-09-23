import { requireAdmin } from "@/lib/auth/roles";
import { ModelManager } from "@/components/model-manager";
import {
  obleth,
  type BoonSettingsView,
  type CacheStats,
  type KnowledgeSettingsView,
  type McpServer,
  type ModelHealthSummary,
} from "@/lib/obleth";
import { boonBlockers } from "@/lib/boon-availability";
import { safe } from "@/lib/safe";
import { loadRecipeCards } from "@/lib/sbatch-recipes";

export const dynamic = "force-dynamic";

export default async function ModelsPage() {
  await requireAdmin();
  const [models, cacheStats, health, mcpServers, slurm, managedSpecs, boonSettings, knowledgeSettings] =
    await Promise.all([
      safe(obleth.listModels(), []),
      safe<CacheStats | undefined>(obleth.cacheStats(), undefined),
      safe<ModelHealthSummary[]>(obleth.modelHealth(), []),
      safe<McpServer[]>(obleth.listMcpServers(), []),
      safe(obleth.getSlurmSettings(), null),
      safe(obleth.listManagedModels(), []),
      safe<BoonSettingsView | null>(obleth.getBoonSettings(), null),
      safe<KnowledgeSettingsView | null>(obleth.getKnowledgeSettings(), null),
    ]);
  // Which models are Slurm-provisioned (have a managed spec). One bulk call —
  // per-model health detail and endpoint lists load lazily when a card is
  // expanded, so the page no longer fans out 3 admin requests per model.
  const managed = Object.fromEntries(managedSpecs.map((spec) => [spec.model_id, true]));

  // Admin-authored *.recipe files and editable DB templates, mapped to flat cards
  // for the create gallery.
  const recipeCards = await loadRecipeCards();

  // A boon grant is inert unless the boon is also switched on globally, so the
  // capabilities form needs to know which ones are unconfigured. Both settings
  // fetches fail open to null, which yields no blockers.
  const blockers = boonBlockers(boonSettings, knowledgeSettings);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Models</h1>
        <p className="text-sm text-muted-foreground">
          Route client model names to upstream inference endpoints. Pod
          selection remains with Aibrix.
        </p>
      </div>
      <ModelManager
        models={models}
        cacheStats={cacheStats}
        health={health}
        mcpServers={mcpServers}
        managed={managed}
        slurmEnabled={slurm?.enabled ?? false}
        recipeCards={recipeCards}
        boonBlockers={blockers}
      />
    </div>
  );
}
