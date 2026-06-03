import { ModelManager } from "@/components/model-manager";
import { obleth, type CacheStats, type ModelHealthDetail, type ModelHealthSummary } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function ModelsPage() {
  const [models, cacheStats, health] = await Promise.all([
    safe(obleth.listModels(), []),
    safe<CacheStats | undefined>(obleth.cacheStats(), undefined),
    safe<ModelHealthSummary[]>(obleth.modelHealth(), []),
  ]);
  const healthDetails = Object.fromEntries(
    await Promise.all(
      models.map(async (model) => {
        const detail = await safe<ModelHealthDetail | undefined>(obleth.modelHealthDetail(model.id), undefined);
        return [model.id, detail] as const;
      }),
    ),
  );

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Models</h1>
        <p className="text-sm text-muted-foreground">
          Route client model names to upstream inference endpoints. Pod selection remains with Aibrix.
        </p>
      </div>
      <ModelManager models={models} cacheStats={cacheStats} health={health} healthDetails={healthDetails} />
    </div>
  );
}
