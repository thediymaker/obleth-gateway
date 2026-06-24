// File-based deployment recipes: a YAML metadata header + the raw `sbatch`
// script an admin already tested. The header carries routing metadata (engine,
// model name, port) and optional placement overrides; the body is submitted
// verbatim as `script_body` while its `#SBATCH` directives are lifted into JSON
// fields (slurmrestd ignores `#SBATCH` — see ./sbatch-directives).
//
// This module owns the new `*.recipe` files (distinct from the former
// wizard yaml definitions).
import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { parse as parseYaml } from "yaml";
import { z } from "zod";
import type { RecipeCard, RecipeDeployPreview } from "@/components/recipes/recipe-card";
import { toRecipeCards } from "@/components/recipes/recipe-card";
import { parseSbatchDirectives, type ParsedDirectives } from "./sbatch-directives";
import type { PutManagedModel } from "@/lib/obleth";
import { obleth } from "@/lib/obleth";

export interface RecipeVariable {
  name: string;
  label?: string;
  default?: string;
  required: boolean;
}

export interface RecipeHeader {
  name: string;
  description?: string;
  engine: string;
  model_type: string;
  api_model_name: string;
  port: number;
  health_path?: string;
  min_replicas?: number;
  target_replicas?: number;
  max_job_failures?: number;
  partition?: string;
  gres?: string;
  cpus_per_task?: number;
  mem?: string;
  time_limit?: string;
  nodes?: number;
  account?: string;
  qos?: string;
  constraints?: string;
  exclude?: string;
  variables?: RecipeVariable[];
}

export interface ParsedRecipe {
  id: string;
  valid: boolean;
  error?: string;
  header?: RecipeHeader;
  body?: string;
  directives?: ParsedDirectives;
  warnings: string[];
}

export function defaultHealthPath(engine: string): string {
  return engine === "ollama" ? "/" : "/health";
}

const VariableSchema = z.object({
  name: z.string().regex(/^[a-zA-Z_][a-zA-Z0-9_]*$/, "invalid variable name"),
  label: z.string().optional(),
  default: z.string().optional(),
  required: z.coerce.boolean().default(false),
});

const HeaderSchema = z
  .object({
    name: z.string().min(1),
    description: z.string().optional(),
    engine: z.string().min(1),
    model_type: z.string().min(1),
    api_model_name: z.string().min(1),
    port: z.coerce.number().int().positive(),
    health_path: z.string().optional(),
    min_replicas: z.coerce.number().int().positive().optional(),
    target_replicas: z.coerce.number().int().positive().default(2),
    max_job_failures: z.coerce.number().int().nonnegative().default(3),
    partition: z.string().optional(),
    gres: z.string().optional(),
    cpus_per_task: z.coerce.number().int().positive().optional(),
    mem: z.string().optional(),
    time_limit: z.string().optional(),
    nodes: z.coerce.number().int().positive().optional(),
    account: z.string().optional(),
    qos: z.string().optional(),
    constraints: z.string().optional(),
    exclude: z.string().optional(),
    variables: z
      .array(VariableSchema)
      .optional()
      .superRefine((vars, ctx) => {
        if (!vars) return;
        const seen = new Set<string>();
        for (const v of vars) {
          if (seen.has(v.name)) {
            ctx.addIssue({ code: z.ZodIssueCode.custom, message: `duplicate variable "${v.name}"` });
          }
          seen.add(v.name);
        }
      }),
  })
  .strip();

/** Split a Jekyll-style `---`\n header \n`---`\n body document. */
function splitFrontmatter(text: string): { header: string; body: string } | null {
  const norm = text.replace(/\r\n/g, "\n");
  if (!norm.startsWith("---\n")) return null;
  // Match a closing fence: exactly "---" on its own line
  const m = norm.slice(4).match(/\n---(?:\r?\n|$)/);
  if (!m || m.index === undefined) return null;
  const fenceStart = 4 + m.index;          // index of the "\n" before "---"
  const header = norm.slice(4, fenceStart);
  const body = norm.slice(fenceStart + m[0].length); // skip past "\n---\n"
  return { header, body };
}

export function parseRecipe(id: string, text: string): ParsedRecipe {
  const split = splitFrontmatter(text);
  if (!split) {
    return { id, valid: false, error: "malformed frontmatter (missing --- fences)", warnings: [] };
  }
  let raw: unknown;
  try {
    raw = parseYaml(split.header);
  } catch (e) {
    return { id, valid: false, error: `invalid YAML header: ${(e as Error).message}`, warnings: [] };
  }
  const parsed = HeaderSchema.safeParse(raw);
  if (!parsed.success) {
    const issue = parsed.error.issues[0];
    const where = issue?.path.join(".") || "header";
    const why = issue?.message ?? "invalid header";
    return { id, valid: false, error: `${where}: ${why}`, warnings: [] };
  }
  const body = split.body.trim();
  if (!body) {
    return { id, valid: false, error: "recipe has no script body", warnings: [] };
  }
  const directives = parseSbatchDirectives(body);
  return { id, valid: true, header: parsed.data, body, directives, warnings: directives.warnings };
}

/** Recipes directory: OBLETH_RECIPES_DIR or ./recipes relative to cwd. */
export function recipesDir(): string {
  const override = process.env.OBLETH_RECIPES_DIR?.trim();
  if (override) return path.resolve(override);
  return path.join(process.cwd(), "recipes");
}

/** Every `*.recipe` in the directory, valid and invalid, sorted by id. Never throws. */
export function listRecipes(): ParsedRecipe[] {
  const dir = recipesDir();
  let entries: string[];
  try {
    if (!statSync(dir).isDirectory()) return [];
    entries = readdirSync(dir);
  } catch {
    return [];
  }
  const out: ParsedRecipe[] = [];
  for (const name of entries.filter((f) => f.endsWith(".recipe")).sort()) {
    const id = name.slice(0, -".recipe".length);
    try {
      out.push(parseRecipe(id, readFileSync(path.join(dir, name), "utf8")));
    } catch (e) {
      out.push({ id, valid: false, error: (e as Error).message, warnings: [] });
    }
  }
  return out;
}

/** One recipe by id (filename stem), or null when no such `*.recipe` file. */
export function getRecipe(id: string): ParsedRecipe | null {
  const full = path.join(recipesDir(), `${id}.recipe`);
  try {
    return parseRecipe(id, readFileSync(full, "utf8"));
  } catch {
    return null;
  }
}

export interface DeployOverrides {
  api_model_name?: string;
  target_replicas?: number;
  /** Deploy-time placement tweaks. A provided string wins over the recipe; an
   *  empty/whitespace string clears the field (sends null/""). `undefined` keeps
   *  the recipe value. */
  qos?: string;
  time_limit?: string;
  partition?: string;
  variables?: Record<string, string>;
}

export interface DeployPayload {
  createBody: {
    model_name: string;
    upstream_model: string;
    api_base: string;
    model_type: string;
  };
  managedBody: PutManagedModel;
}

/** Pick the header value when set, else the parsed `#SBATCH` value. */
function placement<T>(header: T | undefined, parsed: T | undefined): T | undefined {
  return header !== undefined ? header : parsed;
}

/** Apply a deploy-time string override over the recipe value: `undefined` keeps
 *  the recipe value; a provided string wins (trimmed), with empty meaning "clear". */
function overrideString(override: string | undefined, recipeValue: string | undefined): string | undefined {
  if (override === undefined) return recipeValue;
  const trimmed = override.trim();
  return trimmed === "" ? undefined : trimmed;
}

/** Replace declared {{name}} tokens with their resolved values in a single pass.
 *  Only declared names are touched; undeclared {{...}} and all shell ${...}/$(...)
 *  pass through, and substituted text is never re-scanned. Throws when a required
 *  variable has neither a submitted value nor a default. */
function substituteVariables(
  body: string,
  declared: RecipeVariable[] | undefined,
  values: Record<string, string> | undefined,
): string {
  if (!declared || declared.length === 0) return body;
  const resolved = new Map<string, string>();
  for (const v of declared) {
    const value = values?.[v.name]?.trim() || v.default;
    if (value === undefined || value === "") {
      if (v.required) throw new Error(`required variable "${v.name}" has no value`);
      continue; // optional + unset: leave the {{token}} in place
    }
    resolved.set(v.name, value);
  }
  return body.replace(/\{\{([a-zA-Z_][a-zA-Z0-9_]*)\}\}/g, (match, name) =>
    resolved.has(name) ? (resolved.get(name) as string) : match,
  );
}

/** If the recipe declares --chdir, guard the script with a `cd` after the shebang. */
function applyChdir(body: string, chdir: string | undefined): string {
  if (!chdir) return body;
  const guard = `cd '${chdir.replace(/'/g, "'\\''")}' || exit 1`;
  const lines = body.split("\n");
  if (lines[0]?.startsWith("#!")) {
    return [lines[0], guard, ...lines.slice(1)].join("\n");
  }
  return [guard, ...lines].join("\n");
}

/**
 * Turn a valid recipe into the create + managed request bodies obleth expects.
 * Header placement fields override the parsed `#SBATCH` directives. Throws when
 * the recipe is invalid (callers must check `recipe.valid` first).
 */
export function buildManagedFromRecipe(
  recipe: ParsedRecipe,
  overrides: DeployOverrides = {},
): DeployPayload {
  if (!recipe.valid || !recipe.header || !recipe.body || !recipe.directives) {
    throw new Error(`cannot deploy invalid recipe "${recipe.id}": ${recipe.error ?? "unknown"}`);
  }
  const h = recipe.header;
  const d = recipe.directives;
  const modelName = overrides.api_model_name?.trim() || h.api_model_name;
  const targetReplicas = overrides.target_replicas ?? h.target_replicas ?? 2;

  const managedBody: PutManagedModel = {
    enabled: true,
    partition: overrideString(overrides.partition, placement(h.partition, d.partition)) ?? "",
    gres: placement(h.gres, d.gres),
    nodes: placement(h.nodes, d.nodes),
    cpus_per_task: placement(h.cpus_per_task, d.cpus_per_task) ?? null,
    mem: placement(h.mem, d.mem) ?? null,
    time_limit: overrideString(overrides.time_limit, placement(h.time_limit, d.time_limit)) ?? null,
    account: placement(h.account, d.account) ?? null,
    qos: overrideString(overrides.qos, placement(h.qos, d.qos)) ?? null,
    constraints: placement(h.constraints, d.constraints) ?? null,
    exclude: placement(h.exclude, d.exclude) ?? null,
    log_output_dir: d.log_output_dir ?? "",
    image: "",
    preamble: "",
    launch_command: "",
    script_body: applyChdir(
      substituteVariables(recipe.body, h.variables, overrides.variables),
      d.chdir,
    ),
    serving_port: h.port,
    health_path: h.health_path?.trim() || defaultHealthPath(h.engine),
    min_replicas: h.min_replicas ?? 1,
    target_replicas: targetReplicas,
    max_job_failures: h.max_job_failures ?? 3,
    launcher_spec: {
      source: "recipe",
      recipe_id: recipe.id,
      engine: h.engine,
      name: h.name,
    },
  };

  return {
    createBody: {
      model_name: modelName,
      upstream_model: modelName,
      api_base: "",
      model_type: h.model_type,
    },
    managedBody,
  };
}

/** Compute the "what will be deployed" preview, reusing the deploy builder so it
 *  exactly matches the submitted managed body. Returns undefined for invalid recipes. */
export function buildDeployPreview(recipe: ParsedRecipe): RecipeDeployPreview | undefined {
  if (!recipe.valid || !recipe.header) return undefined;
  let payload;
  try {
    payload = buildManagedFromRecipe(recipe);
  } catch {
    return undefined;
  }
  const m = payload.managedBody;
  return {
    apiModelName: payload.createBody.model_name,
    modelType: payload.createBody.model_type,
    engine: recipe.header.engine,
    port: m.serving_port,
    healthPath: m.health_path ?? "/health",
    targetReplicas: m.target_replicas ?? 2,
    maxJobFailures: m.max_job_failures ?? 3,
    partition: m.partition,
    gres: m.gres,
    cpusPerTask: m.cpus_per_task,
    mem: m.mem,
    nodes: m.nodes,
    timeLimit: m.time_limit,
    qos: m.qos,
    account: m.account,
    constraints: m.constraints,
    exclude: m.exclude,
    logOutputDir: m.log_output_dir,
    scriptBody: m.script_body ?? "",
    warnings: recipe.warnings,
    variables: recipe.header.variables,
  };
}

/** Server helper: every recipe as a card, summary + deploy preview.
 *  Merges file-based templates (source:"file") with editable DB templates
 *  (source:"db") fetched from the admin API. The DB fetch is wrapped in a
 *  try/catch so file templates always render even when the admin API is down. */
export async function loadRecipeCards(): Promise<RecipeCard[]> {
  const parsed = listRecipes();
  const cards = toRecipeCards(parsed);
  const fileCards = cards.map((c, i) => ({ ...c, preview: buildDeployPreview(parsed[i]), body: parsed[i].body }));

  let dbCards: RecipeCard[] = [];
  try {
    const rows = await obleth.listRecipes();
    dbCards = rows.map((row) => {
      const parsedRecipe = parseRecipe(row.id, row.body);
      const [card] = toRecipeCards([parsedRecipe]);
      return {
        ...card,
        source: "db" as const,
        recipeId: row.id,
        name: card.name ?? row.name,
        preview: buildDeployPreview(parsedRecipe),
        body: row.body,
      };
    });
  } catch {
    dbCards = []; // admin API unavailable — file templates still render
  }

  return [...fileCards, ...dbCards];
}
