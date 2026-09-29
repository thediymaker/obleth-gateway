// File-based deployment recipes: a YAML metadata header + the raw `sbatch`
// script an admin already tested. The header carries routing metadata (engine,
// model name, port) and optional placement overrides; the body is submitted
// verbatim as `script_body` while its `#SBATCH` directives are lifted into JSON
// fields (slurmrestd ignores `#SBATCH` — see ./sbatch-directives).
//
// This module owns the new `*.recipe` files (distinct from the former
// wizard yaml definitions).
import { readdirSync, readFileSync, realpathSync, statSync } from "node:fs";
import path from "node:path";
import { parse as parseYaml } from "yaml";
import { z } from "zod";
import type { RecipeCard, RecipeDeployPreview } from "@/components/recipes/recipe-card";
import { toRecipeCards } from "@/components/recipes/recipe-card";
import { parseSbatchDirectives, type ParsedDirectives } from "./sbatch-directives";
import { EMPTY_CLUSTER, renderScript, type ClusterValues, type RecipeInput } from "./recipe-inputs";
import { splitFrontmatter } from "./recipe-frontmatter";
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
  /** Folder for job logs; overrides an `#SBATCH --output` directory. */
  log_output_dir?: string;
  /** For a saved recipe: the recipe it was saved from. */
  based_on?: string;
  variables?: RecipeVariable[];
  /** Typed inputs; `variables` are folded in here as text inputs. */
  inputs: RecipeInput[];
  /** "engine": a template for any model (asks for `model`); "model": one model. */
  kind: "model" | "engine";
  /** Hugging Face repo the recipe serves, for the picker and fit check. */
  model?: string;
  /** Weights on disk, in GB, for the fit check. */
  weights_gb?: number;
  /** What the image must provide, e.g. "vLLM 0.22.0 or later". */
  requires?: string;
  /** Node counts offered as "Runs on"; `nodes` is the default. */
  node_options?: number[];
  /** A sentence per node count saying what it means for this model. */
  node_notes?: Record<string, string>;
  /** Environment variables exported at the top of the job. */
  env?: Record<string, string>;
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

const scalar = z.union([z.string(), z.number(), z.boolean()]).transform((v) => String(v));

const InputSchema = z.object({
  name: z.string().regex(/^[a-zA-Z_][a-zA-Z0-9_]*$/, "invalid input name"),
  label: z.string().optional(),
  type: z.enum(["text", "choice", "number", "path", "flag"]).default("text"),
  default: scalar.optional(),
  required: z.coerce.boolean().default(false),
  help: z.string().optional(),
  options: z.array(scalar).optional(),
  min: z.coerce.number().optional(),
  max: z.coerce.number().optional(),
  unit: z.string().optional(),
  adds: z.string().optional(),
  by_nodes: z.record(z.string(), scalar).optional(),
});

function uniqueNames(what: string) {
  return (items: { name: string }[] | undefined, ctx: z.RefinementCtx) => {
    if (!items) return;
    const seen = new Set<string>();
    for (const v of items) {
      if (seen.has(v.name)) ctx.addIssue({ code: z.ZodIssueCode.custom, message: `duplicate ${what} "${v.name}"` });
      seen.add(v.name);
    }
  };
}

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
    log_output_dir: z.string().optional(),
    based_on: z.string().optional(),
    variables: z.array(VariableSchema).optional().superRefine(uniqueNames("variable")),
    inputs: z.array(InputSchema).optional().superRefine(uniqueNames("input")),
    kind: z.enum(["model", "engine"]).default("model"),
    model: z.string().optional(),
    weights_gb: z.coerce.number().positive().optional(),
    requires: z.string().optional(),
    node_options: z.array(z.coerce.number().int().positive()).optional(),
    node_notes: z.record(z.string(), z.string()).optional(),
    env: z.record(z.string().regex(/^[A-Za-z_][A-Za-z0-9_]*$/, "invalid environment variable name"), scalar).optional(),
  })
  .strip()
  .transform((h) => {
    // `variables` (the older, text-only form) become text inputs, so everything
    // downstream deals with one list. A name in both keeps the typed input.
    const typed = h.inputs ?? [];
    const legacy = (h.variables ?? []).filter((v) => !typed.some((i) => i.name === v.name)).map((v): RecipeInput => ({ ...v, type: "text" }));
    return { ...h, inputs: [...typed, ...legacy] };
  });

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

/** `{ id }` for every `*.recipe` file in the directory, sorted by id. Never
 *  throws (a missing/unreadable directory yields []). Shared scan so the two
 *  public listers below can't drift in ordering or directory handling. */
// Recipes are runtime assets, copied explicitly by the Dockerfile or mounted
// through OBLETH_RECIPES_DIR. Do not trace an operator-selected directory into
// the standalone bundle (it can cause the entire source tree to be included).
function recipeFileIds(): { id: string; name: string }[] {
  const dir = recipesDir();
  let entries: string[];
  try {
    if (!statSync(/* turbopackIgnore: true */ dir).isDirectory()) return [];
    entries = readdirSync(/* turbopackIgnore: true */ dir);
  } catch {
    return [];
  }
  return entries
    .filter((f) => f.endsWith(".recipe"))
    .sort()
    .map((name) => ({ id: name.slice(0, -".recipe".length), name }));
}

/** Read only an immediate child of the canonical recipe directory. */
function readRecipeText(id: string): string {
  // IDs are filename stems, never paths. Check both separator styles even on
  // Linux so a saved ID cannot become a traversal when moved to Windows.
  if (!id || id === "." || id === ".." || /[/\\:\0]/.test(id)) {
    throw new Error("Invalid recipe id");
  }
  const root = realpathSync(/* turbopackIgnore: true */ recipesDir());
  const file = realpathSync(/* turbopackIgnore: true */ path.join(/* turbopackIgnore: true */ root, `${id}.recipe`));
  // Resolve symlinks before reading; a link must not escape the recipe root.
  if (path.dirname(file) !== root) throw new Error("Recipe is outside the recipe directory");
  return readFileSync(/* turbopackIgnore: true */ file, "utf8");
}

/** Every `*.recipe` in the directory, valid and invalid, sorted by id. Never throws. */
export function listRecipes(): ParsedRecipe[] {
  const out: ParsedRecipe[] = [];
  for (const { id } of recipeFileIds()) {
    try {
      out.push(parseRecipe(id, readRecipeText(id)));
    } catch (e) {
      out.push({ id, valid: false, error: (e as Error).message, warnings: [] });
    }
  }
  return out;
}

/** Raw text of every `*.recipe` file, paired with its id. Same ordering and
 *  error-handling semantics as `listRecipes` — unreadable files are silently
 *  skipped, never throws. */
export function listRecipeDocs(): { id: string; text: string }[] {
  const out: { id: string; text: string }[] = [];
  for (const { id } of recipeFileIds()) {
    try {
      out.push({ id, text: readRecipeText(id) });
    } catch {
      // skip unreadable files; listRecipes already surfaces them as invalid entries
    }
  }
  return out;
}

/** One recipe by id (filename stem), or null when no such `*.recipe` file. */
export function getRecipe(id: string): ParsedRecipe | null {
  try {
    return parseRecipe(id, readRecipeText(id));
  } catch {
    return null;
  }
}

/** A recipe's full text (header and script) by id, from either source. */
export async function resolveRecipeText(id: string): Promise<string | null> {
  try {
    return readRecipeText(id);
  } catch {
    // not a file recipe
  }
  try {
    const rows = await obleth.listRecipes();
    return rows.find((r) => r.id === id)?.body ?? null;
  } catch {
    return null;
  }
}

/** Resolve a recipe by id across both sources, mirroring `loadRecipeCards`:
 *  file recipes (filename stem) first, then DB templates (UUID id) fetched from
 *  the admin API. Saved templates live only in the database, so callers that
 *  deploy by id (e.g. `deployRecipeAction`) must use this rather than `getRecipe`
 *  alone — otherwise a DB recipe's UUID never matches a `*.recipe` file and the
 *  deploy fails with "recipe not found". Returns null when neither source has it
 *  (the DB lookup is wrapped so an unreachable admin API yields null, not throw). */
export async function resolveRecipeById(id: string): Promise<ParsedRecipe | null> {
  const fileRecipe = getRecipe(id);
  if (fileRecipe) return fileRecipe;
  try {
    const rows = await obleth.listRecipes();
    const row = rows.find((r) => r.id === id);
    if (row) return parseRecipe(row.id, row.body);
  } catch {
    // admin API unavailable — fall through to null
  }
  return null;
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
  account?: string;
  gres?: string;
  mem?: string;
  cpus_per_task?: number | null;
  nodes?: number;
  constraints?: string;
  exclude?: string;
  log_output_dir?: string;
  health_path?: string;
  min_replicas?: number;
  max_job_failures?: number;
  /** Input values by name. `variables` is the older name for the same thing. */
  inputs?: Record<string, string>;
  variables?: Record<string, string>;
  /** Environment for the job, replacing the recipe's `env:` when given. */
  env?: Record<string, string>;
  /** When set, replaces the recipe's script body before variable substitution.
   *  Carries a deploy-time edit (e.g. a fixed image path) through the normal
   *  pipeline so variables and chdir still apply. */
  script_body?: string;
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

/** Export the job's environment right after the shebang (before any chdir
 *  guard is added, so the guard still comes first). Values are single-quoted. */
function applyEnv(body: string, env: Record<string, string> | undefined): string {
  const entries = Object.entries(env ?? {}).filter(([k]) => k.trim());
  if (!entries.length) return body;
  const block = entries.map(([k, v]) => `export ${k}='${v.replace(/'/g, "'\\''")}'`).join("\n");
  const lines = body.split("\n");
  if (lines[0]?.startsWith("#!")) return [lines[0], block, ...lines.slice(1)].join("\n");
  return [block, ...lines].join("\n");
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
  cluster: ClusterValues = EMPTY_CLUSTER,
  { strict = true }: { strict?: boolean } = {},
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
    gres: overrideString(overrides.gres, placement(h.gres, d.gres)),
    nodes: overrides.nodes ?? placement(h.nodes, d.nodes),
    cpus_per_task: overrides.cpus_per_task !== undefined ? overrides.cpus_per_task : placement(h.cpus_per_task, d.cpus_per_task) ?? null,
    mem: overrideString(overrides.mem, placement(h.mem, d.mem)) ?? null,
    time_limit: overrideString(overrides.time_limit, placement(h.time_limit, d.time_limit)) ?? null,
    account: overrideString(overrides.account, placement(h.account, d.account)) ?? null,
    qos: overrideString(overrides.qos, placement(h.qos, d.qos)) ?? null,
    constraints: overrideString(overrides.constraints, placement(h.constraints, d.constraints)) ?? null,
    exclude: overrideString(overrides.exclude, placement(h.exclude, d.exclude)) ?? null,
    log_output_dir: overrideString(overrides.log_output_dir, h.log_output_dir ?? d.log_output_dir ?? (cluster.logs || undefined)) ?? "",
    image: "",
    preamble: "",
    launch_command: "",
    script_body: applyChdir(
      applyEnv(
        renderScript(overrides.script_body ?? recipe.body, h.inputs, overrides.inputs ?? overrides.variables, cluster, {
          strict,
          nodes: overrides.nodes ?? placement(h.nodes, d.nodes),
          builtins: { api_model_name: modelName },
        }),
        overrides.env ?? h.env,
      ),
      d.chdir,
    ),
    serving_port: h.port,
    health_path: overrides.health_path?.trim() || h.health_path?.trim() || defaultHealthPath(h.engine),
    min_replicas: overrides.min_replicas ?? h.min_replicas ?? 1,
    target_replicas: targetReplicas,
    max_job_failures: overrides.max_job_failures ?? h.max_job_failures ?? 3,
    launcher_spec: {
      source: "recipe",
      recipe_id: recipe.id,
      engine: h.engine,
      name: h.name,
      // Kept so "Save as recipe" on the deployment can write the same values back.
      ...(Object.keys(overrides.inputs ?? overrides.variables ?? {}).length ? { inputs: overrides.inputs ?? overrides.variables } : {}),
      ...(overrides.env ? { env: overrides.env } : {}),
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
    // Not strict: an engine recipe's model has no default until someone picks one.
    payload = buildManagedFromRecipe(recipe, {}, EMPTY_CLUSTER, { strict: false });
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
    rawBody: recipe.body ?? "",
    warnings: recipe.warnings,
    variables: recipe.header.variables,
    inputs: recipe.header.inputs,
    kind: recipe.header.kind,
    model: recipe.header.model,
    weightsGb: recipe.header.weights_gb,
    requires: recipe.header.requires,
    nodeOptions: recipe.header.node_options,
    nodeNotes: recipe.header.node_notes,
    basedOn: recipe.header.based_on,
    env: recipe.header.env,
    description: recipe.header.description,
  };
}

/** Server helper: every recipe as a card, summary + deploy preview.
 *  Merges file-based templates (source:"file") with editable DB templates
 *  (source:"db") fetched from the admin API. The DB fetch is wrapped in a
 *  try/catch so file templates always render even when the admin API is down. */
export async function loadRecipeCards(): Promise<RecipeCard[]> {
  const docs = listRecipeDocs();
  const parsed = docs.map((d) => parseRecipe(d.id, d.text));
  const cards = toRecipeCards(parsed);
  const fileCards = cards.map((c, i) => ({
    ...c,
    preview: buildDeployPreview(parsed[i]),
    body: docs[i].text, // FULL raw document (---fenced), matching DB cards
  }));

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
        // The saved Template name (row.name) is what the operator typed in the
        // editor and expects to see; it wins over the recipe's frontmatter
        // `name:`. Fall back to the frontmatter name only if the row name is blank.
        name: row.name || card.name,
        preview: buildDeployPreview(parsedRecipe),
        body: row.body,
      };
    });
  } catch {
    dbCards = []; // admin API unavailable — file templates still render
  }

  return [...fileCards, ...dbCards];
}
