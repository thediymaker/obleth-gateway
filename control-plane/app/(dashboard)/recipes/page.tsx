import { loadRecipeCards } from "@/lib/sbatch-recipes";
import { RecipeList } from "@/components/recipes/recipe-list";

export const dynamic = "force-dynamic";

export default function RecipesPage() {
  const recipes = loadRecipeCards();

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
