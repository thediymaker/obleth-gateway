import { DeploymentsList } from "@/components/deployments/deployments-list";
import { requireAdmin } from "@/lib/auth/roles";
import { loadDeployments } from "@/lib/deployments-data";
import { loadRecipeCards } from "@/lib/sbatch-recipes";

export const dynamic = "force-dynamic";

export default async function DeploymentsPage({ searchParams }: { searchParams: Promise<{ tab?: string; slurm?: string }> }) {
  await requireAdmin();
  const { tab, slurm } = await searchParams;
  const [data, recipes] = await Promise.all([loadDeployments(), loadRecipeCards()]);
  return <DeploymentsList initial={data} recipes={recipes} tab={tab === "recipes" ? "recipes" : "deployments"} slurmSheet={slurm === "1"} />;
}
