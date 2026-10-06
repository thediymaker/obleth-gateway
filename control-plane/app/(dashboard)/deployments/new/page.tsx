import { NewDeployment } from "@/components/deployments/new-deployment";
import { requireAdmin } from "@/lib/auth/roles";
import { obleth, type DeploymentLaunch, type ModelRoute, type SlurmSettingsView } from "@/lib/obleth";
import { clusterValuesFrom } from "@/lib/recipe-inputs";
import { safe } from "@/lib/safe";
import { loadRecipeCards } from "@/lib/sbatch-recipes";

export const dynamic = "force-dynamic";

export default async function NewDeploymentPage({ searchParams }: { searchParams: Promise<{ recipe?: string }> }) {
  await requireAdmin();
  const { recipe } = await searchParams;
  const [recipes, models, slurm, launches] = await Promise.all([
    loadRecipeCards(),
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe<SlurmSettingsView | null>(obleth.getSlurmSettings(), null),
    // Recent launches across all recipes: the form learns from its own recipe's
    // and reads cluster-wide facts (queue waits, failing nodes) from the rest.
    safe(obleth.listDeploymentLaunches({ limit: 500 }), [] as DeploymentLaunch[]),
  ]);
  const taken = models.flatMap((m) => [m.model_name, ...(m.aliases ?? [])]);
  return <NewDeployment recipes={recipes} takenNames={taken} slurmOn={!!slurm?.enabled} initialRecipe={recipe} cluster={clusterValuesFrom(slurm?.cluster_defaults)} launches={launches} />;
}
