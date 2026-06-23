import { listRecipes } from "@/lib/sbatch-recipes";
import { RecipeList } from "@/components/recipes/recipe-list";

export const dynamic = "force-dynamic";

export default function RecipesPage() {
  const recipes = listRecipes().map((r) => ({
    id: r.id,
    valid: r.valid,
    error: r.error,
    name: r.header?.name,
    engine: r.header?.engine,
    modelType: r.header?.model_type,
    description: r.header?.description,
    apiModelName: r.header?.api_model_name,
    targetReplicas: r.header?.target_replicas,
    warnings: r.warnings,
  }));

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Recipes</h1>
        <p className="text-sm text-muted-foreground">
          Deploy an admin-authored{" "}
          <code className="rounded bg-secondary px-1 py-0.5 text-xs">*.recipe</code>{" "}
          file into a managed model.
        </p>
      </div>
      <RecipeList recipes={recipes} />
    </div>
  );
}
