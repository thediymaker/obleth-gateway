import { liveRoute } from "@/lib/auth/guard";
import { loadRecipeCards } from "@/lib/sbatch-recipes";

// The Recipes tab's cards, read when the tab is opened rather than with every
// Deployments list load.
export const GET = liveRoute(() => loadRecipeCards());
