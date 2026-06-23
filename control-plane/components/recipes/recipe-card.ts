// Flat, serializable shape of a recipe for client cards, plus the mapping from
// the server-side ParsedRecipe. Kept separate from recipe-list.tsx so a server
// component can build the cards and pass them across the client boundary.
// `import type` keeps this module free of any runtime dependency on the
// fs-touching sbatch-recipes loader.
import type { ParsedRecipe } from "@/lib/sbatch-recipes";

export interface RecipeCard {
  id: string;
  valid: boolean;
  error?: string;
  name?: string;
  engine?: string;
  modelType?: string;
  description?: string;
  apiModelName?: string;
  targetReplicas?: number;
  warnings: string[];
}

export function toRecipeCards(parsed: ParsedRecipe[]): RecipeCard[] {
  return parsed.map((r) => ({
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
}
