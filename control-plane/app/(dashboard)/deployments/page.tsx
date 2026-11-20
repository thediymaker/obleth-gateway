import { DeploymentsList } from "@/components/deployments/deployments-list";
import { requireAdmin } from "@/lib/auth/roles";
import { loadDeployments } from "@/lib/deployments-data";
import { loadRecipeCards } from "@/lib/sbatch-recipes";

export const dynamic = "force-dynamic";

export default async function DeploymentsPage({ searchParams }: { searchParams: Promise<{ tab?: string; slurm?: string }> }) {
  await requireAdmin();
  const { tab, slurm } = await searchParams;
  const onRecipes = tab === "recipes";
  // The recipes (every one's text and scripts) load only with the Recipes tab;
  // the list reads them when that tab is opened. The 24 h request counts load
  // in the browser, so the page doesn't wait on ClickHouse.
  const [data, recipes] = await Promise.all([loadDeployments(), onRecipes ? loadRecipeCards() : Promise.resolve(undefined)]);
  return <DeploymentsList initial={data} recipes={recipes} tab={onRecipes ? "recipes" : "deployments"} slurmSheet={slurm === "1"} />;
}
