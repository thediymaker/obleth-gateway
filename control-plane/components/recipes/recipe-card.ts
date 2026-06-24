// Flat, serializable shape of a recipe for client cards, plus the mapping from
// the server-side ParsedRecipe. Kept separate from recipe-list.tsx so a server
// component can build the cards and pass them across the client boundary.
// `import type` keeps this module free of any runtime dependency on the
// fs-touching sbatch-recipes loader.
import type { ParsedRecipe, RecipeVariable } from "@/lib/sbatch-recipes";

export interface RecipeDeployPreview {
  apiModelName: string;
  modelType: string;
  engine: string;
  port: number;
  healthPath: string;
  targetReplicas: number;
  maxJobFailures: number;
  partition: string;
  gres?: string;
  cpusPerTask?: number | null;
  mem?: string | null;
  nodes?: number;
  timeLimit?: string | null;
  qos?: string | null;
  account?: string | null;
  constraints?: string | null;
  exclude?: string | null;
  logOutputDir?: string;
  scriptBody: string;
  warnings: string[];
  variables?: RecipeVariable[];
}

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
  preview?: RecipeDeployPreview;
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
