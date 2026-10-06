// Flat, serializable shape of a recipe for client cards, plus the mapping from
// the server-side ParsedRecipe. Kept separate from recipe-list.tsx so a server
// component can build the cards and pass them across the client boundary.
// `import type` keeps this module free of any runtime dependency on the
// fs-touching sbatch-recipes loader.
import type { ParsedRecipe, RecipeTest, RecipeVariable } from "@/lib/sbatch-recipes";
import type { RecipeInput } from "@/lib/recipe-inputs";

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
  /** The recipe's raw script body (placeholders/{{variables}} intact), for the
   *  editable launch textarea. `scriptBody` is the substituted/preview form. */
  rawBody: string;
  warnings: string[];
  variables?: RecipeVariable[];
  inputs: RecipeInput[];
  kind: "model" | "engine";
  model?: string;
  weightsGb?: number;
  requires?: string;
  nodeOptions?: number[];
  nodeNotes?: Record<string, string>;
  basedOn?: string;
  env?: Record<string, string>;
  description?: string;
  tested?: RecipeTest[];
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
  source: "file" | "db";
  recipeId?: string; // DB row id, when source === "db"
  body?: string; // raw recipe text, for pre-filling Edit / Clone-to-edit
}

/** One test of a recipe as a sentence: "Served on 1× GH200 96 GB with vLLM
 *  0.30.1, 2026-10-03." */
export function testedSentence(t: RecipeTest): string {
  return `Served on ${t.hardware}${t.engine ? ` with ${t.engine}` : ""}${t.date ? `, ${t.date}` : ""}.`;
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
    source: "file" as const,
  }));
}
