// File-based deployment recipes: a YAML metadata header + the raw `sbatch`
// script an admin already tested. The header carries routing metadata (engine,
// model name, port) and optional placement overrides; the body is submitted
// as `script_body` while its `#SBATCH` directives are lifted into JSON fields
// and left out of the stored script (slurmrestd ignores `#SBATCH` — see
// ./sbatch-directives).
//
// This module owns the new `*.recipe` files (distinct from the former
// wizard yaml definitions).
import { readdirSync, readFileSync, realpathSync, statSync } from "node:fs";
import path from "node:path";
import { parse as parseYaml } from "yaml";
import { z } from "zod";
import type { RecipeCard, RecipeDeployPreview } from "@/components/recipes/recipe-card";
import { toRecipeCards } from "@/components/recipes/recipe-card";
import { parseSbatchDirectives, stripSbatchDirectives, unappliedAdvice, unixLineEndings, type ParsedDirectives } from "./sbatch-directives";
import { clusterTokens, EMPTY_CLUSTER, inputCapabilities, inputTokens, knownClusterToken, malformedTokens, MODEL_CAPABILITIES, renderScript, type ClusterValues, type ModelCapability, type RecipeInput } from "./recipe-inputs";
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
  /** The first port of each replica's port window. The server must listen on
   *  $OBLETH_SERVING_PORT, which obleth picks inside that window. */
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
  /** The context window the server is started with, in tokens; may name an
   *  input, e.g. "{{context}}". Without it, an input called `context` is used. */
  context_window?: string;
  /** What the model can always do when served this way, e.g. [vision]. */
  capabilities?: ModelCapability[];
  /** Node counts offered as "Runs on"; `nodes` is the default. */
  node_options?: number[];
  /** A sentence per node count saying what it means for this model. */
  node_notes?: Record<string, string>;
  /** Environment variables exported at the top of the job. */
  env?: Record<string, string>;
  /** Where the recipe has been run and served, newest first. */
  tested?: RecipeTest[];
}

/** One run of a recipe that served: the hardware, the engine build, when, and
 *  what was seen (time to ready, speed). */
export interface RecipeTest {
  hardware: string;
  engine?: string;
  date?: string;
  result?: string;
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

/** The engines obleth knows, by their id in a recipe's `engine:`. */
export const KNOWN_ENGINES = ["vllm", "sglang", "llamacpp", "ollama"] as const;
const ENGINE_ALIASES: Record<string, string> = { "llama.cpp": "llamacpp", "llama-cpp": "llamacpp", llama_cpp: "llamacpp", "llama-server": "llamacpp" };

/** A recipe's engine as obleth names it: "llama.cpp" → "llamacpp". */
export function normalizeEngine(engine: string): string {
  const e = engine.trim().toLowerCase();
  return ENGINE_ALIASES[e] ?? e;
}

export function defaultHealthPath(engine: string): string {
  return normalizeEngine(engine) === "ollama" ? "/" : "/health";
}

/** The rule every served model name follows (the same as for a model added by hand). */
export const MODEL_NAME_RE = /^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/;
export const MODEL_NAME_RULE = "use lowercase letters, numbers, dashes and dots, starting and ending with a letter or number";

/** The header keys a recipe may use, in the order the shipped recipes write them. */
export const HEADER_KEYS = [
  "name", "description", "engine", "kind", "model", "weights_gb", "requires",
  "model_type", "api_model_name", "context_window", "capabilities",
  "port", "health_path", "target_replicas", "min_replicas", "max_job_failures",
  "partition", "account", "qos", "time_limit", "nodes", "node_options", "node_notes",
  "gres", "cpus_per_task", "mem", "constraints", "exclude", "log_output_dir",
  "based_on", "env", "tested", "inputs", "variables",
] as const;
const INPUT_KEYS = ["name", "label", "type", "default", "required", "help", "options", "min", "max", "unit", "adds", "capabilities", "drops", "by_nodes"] as const;
const VARIABLE_KEYS = ["name", "label", "default", "required"] as const;
const TEST_KEYS = ["hardware", "engine", "date", "result"] as const;

/** Common slips, for the "did you mean" hint. */
const KEY_HINTS: Record<string, string> = {
  time: "time_limit", walltime: "time_limit", health: "health_path", gpus: "gres", gpu: "gres",
  cpus: "cpus_per_task", memory: "mem", replicas: "target_replicas", constraint: "constraints",
  output: "log_output_dir", log_dir: "log_output_dir", logs: "log_output_dir", served_model_name: "api_model_name",
  model_name: "api_model_name", context: "context_window", max_model_len: "context_window",
  capability: "capabilities", input: "inputs", environment: "env", option: "options", choices: "options",
};

function unknownKeyProblem(where: string, key: string, allowed: readonly string[]): string {
  const norm = key.trim().toLowerCase().replace(/-/g, "_");
  const hint = allowed.includes(norm) ? norm : KEY_HINTS[norm] && allowed.includes(KEY_HINTS[norm]) ? KEY_HINTS[norm] : null;
  return `${where}: "${key}" isn't a recipe setting${hint ? `; did you mean ${hint}?` : "."}`;
}

/** Unknown keys in the header and its lists, as sentences. */
function unknownKeys(raw: unknown): string[] {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return [];
  const out: string[] = [];
  const h = raw as Record<string, unknown>;
  for (const k of Object.keys(h)) if (!(HEADER_KEYS as readonly string[]).includes(k)) out.push(unknownKeyProblem("header", k, HEADER_KEYS));
  const lists: [string, readonly string[]][] = [["inputs", INPUT_KEYS], ["variables", VARIABLE_KEYS], ["tested", TEST_KEYS]];
  for (const [list, keys] of lists) {
    const items = h[list];
    if (!Array.isArray(items)) continue;
    items.forEach((item, i) => {
      if (!item || typeof item !== "object") return;
      const name = (item as { name?: unknown }).name;
      const where = `${list}.${i}${typeof name === "string" ? ` (${name})` : ""}`;
      for (const k of Object.keys(item)) if (!keys.includes(k)) out.push(unknownKeyProblem(where, k, keys));
    });
  }
  return out;
}

// YAML 1.2 reads `no` and "false" as strings, and coercing them would make
// them true; accept the spellings people write and refuse anything else.
const yesNo = z.union([
  z.boolean(),
  z.enum(["true", "false", "yes", "no", "on", "off", "True", "False", "Yes", "No"]).transform((v) => ["true", "yes", "on"].includes(v.toLowerCase())),
], { errorMap: () => ({ message: "must be true or false" }) });

const VariableSchema = z.object({
  name: z.string().regex(/^[a-zA-Z_][a-zA-Z0-9_]*$/, "invalid variable name"),
  label: z.string().optional(),
  default: z.string().optional(),
  required: yesNo.default(false),
});

const scalar = z.union([z.string(), z.number(), z.boolean()]).transform((v) => String(v));

// YAML reads an unquoted 2026-10-03 as a date; keep it as the text written.
const day = z.union([z.string(), z.date()]).transform((v) => (typeof v === "string" ? v : v.toISOString().slice(0, 10)));

const TestSchema = z.object({
  hardware: z.string().min(1),
  engine: z.string().optional(),
  date: day.optional(),
  result: z.string().optional(),
});

const capabilityList = z.array(z.enum(MODEL_CAPABILITIES, { errorMap: () => ({ message: `must be one of ${MODEL_CAPABILITIES.join(", ")}` }) }));

const InputSchema = z.object({
  name: z.string().regex(/^[a-zA-Z_][a-zA-Z0-9_]*$/, "invalid input name"),
  label: z.string().optional(),
  type: z.enum(["text", "choice", "number", "path", "flag"]).default("text"),
  default: scalar.optional(),
  required: yesNo.default(false),
  help: z.string().optional(),
  options: z.array(scalar).optional(),
  min: z.coerce.number().optional(),
  max: z.coerce.number().optional(),
  unit: z.string().optional(),
  adds: z.string().optional(),
  capabilities: capabilityList.optional(),
  drops: capabilityList.optional(),
  by_nodes: z.record(z.string(), scalar).optional(),
}).superRefine((i, ctx) => {
  const problem = (message: string) => ctx.addIssue({ code: z.ZodIssueCode.custom, message: `${i.name}: ${message}` });
  if ((i.capabilities?.length || i.drops?.length) && i.type !== "flag") problem("capabilities and drops only go on a flag input");
  if (i.type === "flag" && i.adds === undefined) problem("a flag needs adds: the text it puts in the script when it's on");
  if (i.type === "flag" && i.default !== undefined && !["true", "false"].includes(i.default)) problem("a flag's default must be true or false");
  if (i.options && i.type !== "choice") problem("options only go on a choice input");
  if (i.type === "choice") {
    if (!i.options?.length) problem("a choice needs options");
    else if (i.default !== undefined && !i.default.includes("{{") && !i.options.includes(i.default)) problem(`the default ${i.default} isn't one of the options (${i.options.join(", ")})`);
  }
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
    engine: z.string().min(1).transform(normalizeEngine),
    model_type: z.string().min(1),
    api_model_name: z.string().min(1).regex(MODEL_NAME_RE, `the served name must ${MODEL_NAME_RULE}`),
    // The base of each replica's port window, not a port the server listens on.
    port: z.coerce.number().int().min(1).max(65535).default(8000),
    health_path: z.string().startsWith("/", "must start with /").optional(),
    min_replicas: z.coerce.number().int().positive().optional(),
    target_replicas: z.coerce.number().int().nonnegative().default(1),
    max_job_failures: z.coerce.number().int().nonnegative().default(2),
    partition: z.string().optional(),
    gres: z.string().optional(),
    cpus_per_task: z.coerce.number().int().positive().optional(),
    mem: scalar.optional(),
    time_limit: scalar.optional(),
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
    context_window: scalar.optional(),
    capabilities: capabilityList.optional(),
    node_options: z.array(z.coerce.number().int().positive()).optional(),
    node_notes: z.record(z.string(), z.string()).optional(),
    env: z.record(z.string().regex(/^[A-Za-z_][A-Za-z0-9_]*$/, "invalid environment variable name"), scalar).optional(),
    tested: z.array(TestSchema).optional(),
  })
  .strip()
  .transform((h) => {
    // `variables` (the older, text-only form) become text inputs, so everything
    // downstream deals with one list. A name in both keeps the typed input.
    const typed = h.inputs ?? [];
    const legacy = (h.variables ?? []).filter((v) => !typed.some((i) => i.name === v.name)).map((v): RecipeInput => ({ ...v, type: "text" }));
    return { ...h, inputs: [...typed, ...legacy] };
  });

/** Names {{api_model_name}} and the like that obleth fills without an input. */
const BUILTIN_TOKENS = ["api_model_name"];

/** What's wrong with a parsed recipe's script and token use, as sentences
 *  (refused), and what is merely odd (warnings). */
function scriptProblems(h: RecipeHeader, body: string, directives: ParsedDirectives): { errors: string[]; warnings: string[] } {
  const errors: string[] = [];
  const warnings: string[] = [];
  const declared = new Set([...h.inputs.map((i) => i.name), ...BUILTIN_TOKENS]);
  const texts = [body, h.context_window ?? "", ...h.inputs.map((i) => `${i.default ?? ""} ${Object.values(i.by_nodes ?? {}).join(" ")}`)];
  for (const name of inputTokens(texts.join("\n"))) {
    if (!declared.has(name)) errors.push(`The recipe uses {{${name}}}, but no input is called ${name}. Declare it under inputs, or correct the name.`);
  }
  for (const t of malformedTokens(texts.join("\n"))) errors.push(`${t} isn't a token obleth can fill. Write {{name}} for an input or {{cluster.cache}}-style names for a cluster default.`);
  for (const t of clusterTokens(texts.join("\n"))) {
    if (!knownClusterToken(t.key, t.sub)) errors.push(`{{${t.text}}} isn't a cluster default. Use cluster.cache, cluster.images, cluster.logs, cluster.setup or cluster.image.<engine>.`);
  }
  for (const [k, v] of Object.entries(h.env ?? {})) {
    if (/\{\{/.test(v)) errors.push(`The env value for ${k} uses a {{token}}, but env values are exported exactly as written. Set it in the script instead, e.g. export ${k}="{{...}}".`);
  }
  if (h.context_window !== undefined && !/\{\{/.test(h.context_window) && !/^\d+$/.test(h.context_window.trim())) {
    errors.push(`context_window must be a number of tokens or an input such as {{context}}, not ${h.context_window}.`);
  }
  const code = body.split("\n").filter((l) => !/^\s*#/.test(l)).join("\n");
  if (!/\$\{?OBLETH_SERVING_PORT\b/.test(code)) {
    errors.push('The script never uses $OBLETH_SERVING_PORT. Start the server on that port, e.g. --port "$OBLETH_SERVING_PORT": obleth picks a free port for each replica and checks only that one.');
  }
  const fixed = /(?:^|\s)--port(?:=|\s+)["']?(\d+)/m.exec(code);
  if (fixed) errors.push(`The script starts a server on the fixed port ${fixed[1]}. Use --port "$OBLETH_SERVING_PORT" so each replica gets its own port.`);
  for (const flag of directives.unapplied ?? []) errors.push(`#SBATCH ${unappliedAdvice(flag)}`);
  for (const i of h.inputs) {
    // A flag that only marks what the model can do needn't appear in the script.
    if (i.type === "flag" && !i.adds && (i.capabilities?.length || i.drops?.length)) continue;
    if (!inputTokens(`${body}\n${h.context_window ?? ""}`).includes(i.name)) warnings.push(`The input ${i.name} isn't used in the script, so changing it does nothing.`);
  }
  if (!(KNOWN_ENGINES as readonly string[]).includes(h.engine)) {
    warnings.push(`obleth doesn't know the engine ${h.engine}. The recipe launches as written, but it isn't offered for Hugging Face models.`);
  }
  return { errors, warnings };
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
  const unknown = unknownKeys(raw);
  if (unknown.length) return { id, valid: false, error: unknown.join(" "), warnings: [] };
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
  const header = parsed.data as RecipeHeader;
  const checked = scriptProblems(header, body, directives);
  if (checked.errors.length) return { id, valid: false, error: checked.errors.join(" "), header, body, directives, warnings: checked.warnings };
  return { id, valid: true, header, body, directives, warnings: [...directives.warnings, ...checked.warnings] };
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
  /** What kind of model it is (chat, embedding, …), when the launch knows
   *  better than the recipe, e.g. an engine recipe serving an embedding model. */
  model_type?: string;
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
    /** The context the server is started with, so the gateway's limits match. */
    context_window?: number;
  } & Partial<Record<`supports_${ModelCapability}`, true>>;
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
  // The name goes into the script as the served name, so it must be one word
  // the engine and the shell read the same way.
  if (!MODEL_NAME_RE.test(modelName)) throw new Error(`The name ${modelName} can't be used: names ${MODEL_NAME_RULE}.`);
  const targetReplicas = overrides.target_replicas ?? h.target_replicas ?? 1;
  const nodes = overrides.nodes ?? placement(h.nodes, d.nodes);
  const values = overrides.inputs ?? overrides.variables;
  const rawScript = unixLineEndings(overrides.script_body ?? recipe.body);
  const rendered = renderScript(rawScript, h.inputs, values, cluster, { strict, nodes, builtins: { api_model_name: modelName } });

  const managedBody: PutManagedModel = {
    enabled: true,
    partition: overrideString(overrides.partition, placement(h.partition, d.partition)) ?? "",
    gres: overrideString(overrides.gres, placement(h.gres, d.gres)),
    nodes,
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
    // The #SBATCH lines are already in the fields above, and Slurm doesn't
    // read them from a submitted script, so the stored script leaves them out.
    script_body: applyChdir(
      applyEnv(stripSbatchDirectives(rendered), overrides.env ?? h.env),
      d.chdir,
    ),
    serving_port: h.port,
    health_path: overrides.health_path?.trim() || h.health_path?.trim() || defaultHealthPath(h.engine),
    min_replicas: overrides.min_replicas ?? h.min_replicas ?? 1,
    target_replicas: targetReplicas,
    max_job_failures: overrides.max_job_failures ?? h.max_job_failures ?? 2,
    launcher_spec: {
      source: "recipe",
      recipe_id: recipe.id,
      engine: h.engine,
      name: h.name,
      // Kept so "Save as recipe" on the deployment can write the same values back.
      ...(Object.keys(overrides.inputs ?? overrides.variables ?? {}).length ? { inputs: overrides.inputs ?? overrides.variables } : {}),
      ...(overrides.env ? { env: overrides.env } : {}),
      // A script edited at launch, kept so "Save as recipe" saves that version.
      ...(overrides.script_body !== undefined && overrides.script_body !== recipe.body ? { script: rawScript } : {}),
    },
  };
  // Kept to tell, when saving as a recipe, whether the script was changed on
  // the deployment's page after launch.
  (managedBody.launcher_spec as Record<string, unknown>).script_digest = scriptDigest(managedBody.script_body ?? "");

  // What the flags that are on tell us the model can do, e.g. a tool parser
  // means function calling, so its tool boons work without a second step.
  const capabilities = inputCapabilities(h.inputs, values, cluster, nodes, { always: h.capabilities, script: rendered });
  const contextWindow = servedContext(h, values, cluster, nodes);

  return {
    createBody: {
      model_name: modelName,
      upstream_model: modelName,
      api_base: "",
      model_type: overrides.model_type?.trim() || h.model_type,
      ...(contextWindow ? { context_window: contextWindow } : {}),
      ...Object.fromEntries(capabilities.map((c) => [`supports_${c}`, true])),
    },
    managedBody,
  };
}

/** The context window the recipe starts the server with: `context_window:`
 *  (which may name inputs), else the value of an input called `context`.
 *  Undefined when neither gives a whole number of tokens. */
export function servedContext(h: RecipeHeader, values: Record<string, string> | undefined, cluster: ClusterValues, nodes?: number): number | undefined {
  const source = h.context_window ?? (h.inputs.some((i) => i.name === "context") ? "{{context}}" : undefined);
  if (source === undefined) return undefined;
  const text = renderScript(source, h.inputs, values, cluster, { nodes }).trim();
  return /^\d+$/.test(text) && Number(text) > 0 ? Number(text) : undefined;
}

/** A short, stable fingerprint of a script (FNV-1a, hex). Not a security
 *  measure: it only tells whether the text changed. */
export function scriptDigest(text: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h.toString(16).padStart(8, "0");
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
    targetReplicas: m.target_replicas ?? 1,
    maxJobFailures: m.max_job_failures ?? 2,
    contextWindow: payload.createBody.context_window,
    capabilities: MODEL_CAPABILITIES.filter((c) => (payload.createBody as Record<string, unknown>)[`supports_${c}`] === true),
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
    tested: recipe.header.tested,
  };
}

/** Server helper: every recipe as a card, summary + deploy preview.
 *  Merges file-based templates (source:"file") with editable DB templates
 *  (source:"db") fetched from the admin API. The DB fetch is wrapped in a
 *  try/catch so file templates always render even when the admin API is down. */
export async function loadRecipeCards(): Promise<RecipeCard[]> {
  // Every file, readable or not: one that can't be read shows as a card saying
  // why, instead of the library quietly being short.
  const docs: { id: string; text: string | null; error?: string }[] = recipeFileIds().map(({ id }) => {
    try {
      return { id, text: readRecipeText(id) };
    } catch (e) {
      return { id, text: null, error: `Can't be read: ${(e as Error).message}` };
    }
  });
  const parsed = docs.map((d): ParsedRecipe => (d.text === null ? { id: d.id, valid: false, error: d.error, warnings: [] } : parseRecipe(d.id, d.text)));
  const cards = toRecipeCards(parsed);
  const fileCards = cards.map((c, i) => ({
    ...c,
    preview: buildDeployPreview(parsed[i]),
    ...(docs[i].text !== null ? { body: docs[i].text as string } : {}), // FULL raw document (---fenced), matching DB cards
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
