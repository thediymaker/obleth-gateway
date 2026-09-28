import { redirect } from "next/navigation";
import { ModelsList } from "@/components/models/models-list";
import type { AddMode } from "@/components/models/add-model";
import { requireAdmin } from "@/lib/auth/roles";
import { modelHref } from "@/lib/models-model";
import { obleth, type CacheStats, type ModelHealthSummary } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { loadRecipeCards } from "@/lib/sbatch-recipes";

export const dynamic = "force-dynamic";

const ADD_MODES: AddMode[] = ["connect", "provider", "file", "slurm"];

export default async function ModelsPage({ searchParams }: { searchParams: Promise<{ model?: string; add?: string }> }) {
  await requireAdmin();
  const params = await searchParams;
  // Older links opened a model inline with `?model=`; each model has its own page now.
  if (params.model) redirect(modelHref(params.model));

  const [models, cacheStats, health, slurm, managedSpecs, recipeCards] = await Promise.all([
    safe(obleth.listModels(), []),
    safe<CacheStats | undefined>(obleth.cacheStats(), undefined),
    safe<ModelHealthSummary[]>(obleth.modelHealth(), []),
    safe(obleth.getSlurmSettings(), null),
    safe(obleth.listManagedModels(), []),
    loadRecipeCards(),
  ]);
  const managed = Object.fromEntries(managedSpecs.map((spec) => [spec.model_id, true]));
  const add = ADD_MODES.find((m) => m === params.add) ?? (params.add ? "connect" : null);

  return (
    <ModelsList
      models={models}
      health={health}
      managed={managed}
      cacheStats={cacheStats}
      slurmEnabled={slurm?.enabled ?? false}
      recipeCards={recipeCards}
      initialAdd={add}
    />
  );
}
