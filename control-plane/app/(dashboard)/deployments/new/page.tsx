import { NewDeployment } from "@/components/deployments/new-deployment";
import { requireAdmin } from "@/lib/auth/roles";
import { obleth, type ModelRoute, type SlurmSettingsView } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { loadRecipeCards } from "@/lib/sbatch-recipes";

export const dynamic = "force-dynamic";

export default async function NewDeploymentPage({ searchParams }: { searchParams: Promise<{ recipe?: string }> }) {
  await requireAdmin();
  const { recipe } = await searchParams;
  const [recipes, models, slurm] = await Promise.all([
    loadRecipeCards(),
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe<SlurmSettingsView | null>(obleth.getSlurmSettings(), null),
  ]);
  const taken = models.flatMap((m) => [m.model_name, ...(m.aliases ?? [])]);
  return <NewDeployment recipes={recipes} takenNames={taken} slurmOn={!!slurm?.enabled} initialRecipe={recipe} />;
}
