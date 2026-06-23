import { ModelManager } from "@/components/model-manager";
import {
  obleth,
  type CacheStats,
  type McpServer,
  type ModelEndpoint,
  type ModelHealthDetail,
  type ModelHealthSummary,
} from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { loadRecipeCards } from "@/lib/sbatch-recipes";

export const dynamic = "force-dynamic";

export default async function ModelsPage() {
  const [models, cacheStats, health, mcpServers, slurm] = await Promise.all([
    safe(obleth.listModels(), []),
    safe<CacheStats | undefined>(obleth.cacheStats(), undefined),
    safe<ModelHealthSummary[]>(obleth.modelHealth(), []),
    safe<McpServer[]>(obleth.listMcpServers(), []),
    safe(obleth.getSlurmSettings(), null),
  ]);
  // Fetch each model's health detail and endpoint list concurrently in a
  // single fan-out, so the page waits for the slowest request once instead of
  // two sequential per-model batches.
  const perModel = await Promise.all(
    models.map(async (model) => {
      const [detail, endpointList, managedSpec] = await Promise.all([
        safe<ModelHealthDetail | undefined>(
          obleth.modelHealthDetail(model.id),
          undefined,
        ),
        safe<ModelEndpoint[]>(obleth.listModelEndpoints(model.id), []),
        safe(obleth.getManagedModel(model.id), null),
      ]);
      return [model.id, detail, endpointList, managedSpec] as const;
    }),
  );
  const healthDetails = Object.fromEntries(
    perModel.map(([id, detail]) => [id, detail]),
  );
  const endpoints = Object.fromEntries(
    perModel.map(([id, , endpointList]) => [id, endpointList]),
  );
  // Which models are Slurm-provisioned (have a managed spec). Used to show the
  // Provisioning tab only for those models.
  const managed = Object.fromEntries(
    perModel.map(([id, , , spec]) => [id, spec !== null]),
  );

  // Admin-authored *.recipe files, mapped to flat cards for the create gallery.
  const recipeCards = loadRecipeCards();

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
        healthDetails={healthDetails}
        endpoints={endpoints}
        mcpServers={mcpServers}
        managed={managed}
        slurmEnabled={slurm?.enabled ?? false}
        recipeCards={recipeCards}
      />
    </div>
  );
}
