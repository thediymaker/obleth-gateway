// Typed recipe inputs and cluster values. Pure and client-safe: the launch form
// draws and checks inputs with it, and the server renders the job script with
// the same functions, so what the review shows is what gets submitted.
//
// A recipe's `inputs:` declare the few settings someone launching it may
// change. Each is substituted into the script as {{name}}. Defaults may name a
// cluster value ({{cluster.cache}}, {{cluster.image.vllm}}, …), which comes
// from the Slurm connection's cluster defaults, so no recipe hard-codes a path.

/** The rule every served model name follows (the same as for a model added
 *  by hand): it goes into job scripts as one shell word. */
export const MODEL_NAME_RE = /^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/;
export const MODEL_NAME_RULE = "use lowercase letters, numbers, dashes and dots, starting and ending with a letter or number";

export type InputType = "text" | "choice" | "number" | "path" | "flag";

/** The model flags a recipe input can turn on, named as in a recipe. */
export const MODEL_CAPABILITIES = ["function_calling", "tool_choice", "response_schema", "vision"] as const;
export type ModelCapability = (typeof MODEL_CAPABILITIES)[number];

export interface RecipeInput {
  name: string;
  label?: string;
  type: InputType;
  /** Always a string; flags use "true"/"false". May contain {{cluster.*}}. */
  default?: string;
  required: boolean;
  help?: string;
  /** choice: the allowed values, in display order. */
  options?: string[];
  /** number: inclusive bounds. */
  min?: number;
  max?: number;
  /** number: unit shown after the box, e.g. "GiB per node". */
  unit?: string;
  /** flag: what {{name}} becomes when on (empty when off). */
  adds?: string;
  /** flag: what the launched model can do when it's on, e.g. a tool parser
   *  means function calling. Set on the model it creates. */
  capabilities?: ModelCapability[];
  /** flag: what the launched model can no longer do when it's on, e.g. a
   *  text-only switch drops vision. Wins over the recipe's `capabilities`. */
  drops?: ModelCapability[];
  /** Default per node count ("1" → "270"), for values that follow Runs on. */
  by_nodes?: Record<string, string>;
}

/** The cluster-wide values recipes refer to as {{cluster.*}}. */
export interface ClusterValues {
  cache: string;
  images: string;
  logs: string;
  setup: string;
  /** Engine → image path, or a file name under `images`. */
  image: Record<string, string>;
}

export const EMPTY_CLUSTER: ClusterValues = { cache: "", images: "", logs: "", setup: "", image: {} };

const CLUSTER_TOKEN = /\{\{\s*cluster\.([a-z_]+)(?:\.([a-zA-Z0-9_.-]+))?\s*\}\}/g;
const INPUT_TOKEN = /\{\{\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\}\}/g;
/** Any {{...}} token, for finding ones nothing fills. */
const ANY_TOKEN = /\{\{[^{}]*\}\}/g;

/** The cluster values a recipe may name, for the loader's check. */
export function knownClusterToken(key: string, sub?: string): boolean {
  if (key === "image") return !!sub;
  return !sub && (key === "cache" || key === "images" || key === "logs" || key === "setup");
}

/** Every {{cluster.*}} token in a text, as written ("cluster.image.vllm"). */
export function clusterTokens(text: string): { key: string; sub?: string; text: string }[] {
  return [...text.matchAll(CLUSTER_TOKEN)].map((m) => ({ key: m[1], sub: m[2], text: m[2] ? `cluster.${m[1]}.${m[2]}` : `cluster.${m[1]}` }));
}

/** The input names a text refers to as {{name}} (cluster tokens excluded). */
export function inputTokens(text: string): string[] {
  return [...new Set([...text.matchAll(INPUT_TOKEN)].map((m) => m[1]))];
}

/** {{...}} tokens that look like a recipe token (they start with a letter) but
 *  are neither an input name nor a cluster value, as written. Other braces,
 *  such as a Go template's {{.ID}}, are left alone. */
export function malformedTokens(text: string): string[] {
  return [...new Set((text.match(ANY_TOKEN) ?? []).filter((t) => /^\{\{\s*[A-Za-z_]/.test(t) && !/^\{\{\s*[a-zA-Z_][a-zA-Z0-9_]*\s*\}\}$/.test(t) && !/^\{\{\s*cluster\.[a-z_]+(?:\.[a-zA-Z0-9_.-]+)?\s*\}\}$/.test(t)))];
}

/** Join an image name onto the images folder unless it is already a path. */
function imagePath(value: string, dir: string): string {
  if (!value || value.startsWith("/") || value.includes("://") || !dir) return value;
  return `${dir.replace(/\/+$/, "")}/${value}`;
}

/** One cluster value, or undefined when it isn't set. */
export function clusterValue(cv: ClusterValues, key: string, sub?: string): string | undefined {
  // Setup lines are optional everywhere: unset means nothing to run.
  if (key === "setup" && !sub) return cv.setup ?? "";
  let v: string | undefined;
  if (key === "image" && sub) v = cv.image[sub] ? imagePath(cv.image[sub], cv.images) : undefined;
  else if (key === "cache" || key === "images" || key === "logs") v = cv[key];
  return v?.trim() ? v : undefined;
}

/** Replace the {{cluster.*}} tokens that have a value; unset ones stay put. */
export function resolveClusterTokens(text: string, cv: ClusterValues): string {
  return text.replace(CLUSTER_TOKEN, (m, key: string, sub?: string) => clusterValue(cv, key, sub) ?? m);
}

/** The {{cluster.*}} tokens in a text that have no value on this cluster. */
export function missingClusterValues(text: string, cv: ClusterValues): string[] {
  const out = new Set<string>();
  for (const m of text.matchAll(CLUSTER_TOKEN)) {
    if (clusterValue(cv, m[1], m[2]) === undefined) out.add(m[2] ? `cluster.${m[1]}.${m[2]}` : `cluster.${m[1]}`);
  }
  return [...out];
}

/** Where an input's starting value comes from, for the form's source tag. */
export function inputSource(input: RecipeInput): "cluster" | "recipe" {
  return /\{\{\s*cluster\./.test(input.default ?? "") ? "cluster" : "recipe";
}

/** An input's default for a node count, with cluster values filled in. An
 *  optional input whose cluster value isn't set starts empty; a required one
 *  keeps the token so the form can say which cluster default is missing. */
export function inputDefault(input: RecipeInput, cv: ClusterValues, nodes?: number): string {
  const raw = (nodes != null ? input.by_nodes?.[String(nodes)] : undefined) ?? input.default ?? (input.type === "flag" ? "false" : "");
  const v = resolveClusterTokens(raw, cv);
  return !input.required && /\{\{\s*cluster\./.test(v) ? "" : v;
}

/** Every input's starting value. */
export function inputDefaults(inputs: RecipeInput[], cv: ClusterValues, nodes?: number): Record<string, string> {
  return Object.fromEntries(inputs.map((i) => [i.name, inputDefault(i, cv, nodes)]));
}

function isOn(v: string | undefined): boolean {
  return v === "true" || v === "on" || v === "1" || v === "yes";
}

/** Why a value can't be used for an input, or null when it can. */
export function inputProblem(input: RecipeInput, value: string | undefined): string | null {
  const v = (value ?? "").trim();
  const label = input.label || input.name;
  if (!v) return input.required && input.type !== "flag" ? `${label} is needed` : null;
  if (/\{\{\s*cluster\./.test(v)) return `${label} uses a cluster default that isn't set`;
  switch (input.type) {
    case "number": {
      const n = Number(v);
      if (!Number.isFinite(n)) return `${label} must be a number`;
      if (input.min != null && n < input.min) return `${label} must be at least ${input.min}`;
      if (input.max != null && n > input.max) return `${label} must be at most ${input.max}`;
      return null;
    }
    case "choice":
      return input.options?.length && !input.options.includes(v) ? `${label} must be one of ${input.options.join(", ")}` : null;
    case "flag":
      return ["true", "false", "on", "off", "1", "0", "yes", "no"].includes(v) ? null : `${label} must be on or off`;
    default:
      return null;
  }
}

/** Every problem with a set of values, in input order. */
export function inputProblems(inputs: RecipeInput[], values: Record<string, string>): string[] {
  return inputs.map((i) => inputProblem(i, values[i.name])).filter((p): p is string => !!p);
}

/** What a rendered job script turns on by its engine flags alone: a tool
 *  parser (vLLM, SGLang) or llama-server's --jinja templates mean tool calls,
 *  wherever the flag came from (a flag input, the free-text extras, the script). */
export function scriptCapabilities(script: string): ModelCapability[] {
  // Comments don't count: a commented-out flag isn't passed.
  const code = script.split("\n").filter((l) => !/^\s*#/.test(l)).join("\n");
  const tools = /(^|\s)--(tool-call-parser|enable-auto-tool-choice)(\s|=|$)/m.test(code) || (/\bllama-server\b/.test(code) && /(^|\s)--jinja(\s|$)/m.test(code));
  return tools ? ["function_calling", "tool_choice"] : [];
}

/** The capabilities the launched model gets, in MODEL_CAPABILITIES order: the
 *  recipe's always-on `capabilities`, those of flags that are on, and those the
 *  rendered script's engine flags imply, minus what an on flag `drops`. A flag
 *  left unset follows its default. */
export function inputCapabilities(
  inputs: RecipeInput[],
  values: Record<string, string> | undefined,
  cv: ClusterValues,
  nodes?: number,
  extra: { always?: readonly ModelCapability[]; script?: string } = {},
): ModelCapability[] {
  const on = new Set<ModelCapability>(extra.always ?? []);
  if (extra.script) for (const c of scriptCapabilities(extra.script)) on.add(c);
  const dropped = new Set<ModelCapability>();
  for (const input of inputs) {
    if (input.type !== "flag" || (!input.capabilities?.length && !input.drops?.length)) continue;
    const given = values?.[input.name]?.trim();
    if (!isOn(given ? given : inputDefault(input, cv, nodes))) continue;
    for (const c of input.capabilities ?? []) on.add(c);
    for (const c of input.drops ?? []) dropped.add(c);
  }
  return MODEL_CAPABILITIES.filter((c) => on.has(c) && !dropped.has(c));
}

/** What {{name}} becomes for one input and value. */
function rendered(input: RecipeInput, value: string): string {
  if (input.type === "flag") return isOn(value) ? input.adds ?? "" : "";
  return value;
}

/**
 * Fill a script: each declared {{input}} gets its value (or default), then the
 * {{cluster.*}} tokens get the cluster's values. An optional input with no
 * value becomes empty, never a literal {{name}}. A value given as empty stays
 * empty (the default is used only when no value is given), except for a flag,
 * which has no empty state. Shell ${…}/$(…) pass through, and filled-in text is
 * never re-scanned for inputs. With `strict` (the server's launch path), a
 * required input without a value, or any {{…}} left unfilled, throws; without
 * it, the token is left for the review to show.
 */
export function renderScript(
  body: string,
  inputs: RecipeInput[],
  values: Record<string, string> | undefined,
  cv: ClusterValues,
  opts: { strict?: boolean; nodes?: number; builtins?: Record<string, string> } = {},
): string {
  const resolved = new Map<string, string>();
  for (const input of inputs) {
    const given = values?.[input.name];
    const value = given !== undefined && (given.trim() !== "" || input.type !== "flag") ? given.trim() : inputDefault(input, cv, opts.nodes);
    if (!value && input.type !== "flag") {
      if (input.required && opts.strict) throw new Error(`required input "${input.name}" has no value`);
      // An optional input left empty adds nothing; a required one keeps its
      // token so the review shows what still needs a value.
      if (!input.required) resolved.set(input.name, "");
      continue;
    }
    resolved.set(input.name, resolveClusterTokens(rendered(input, value), cv));
  }
  // Built-ins ({{api_model_name}}) fill tokens no input declares.
  for (const [k, v] of Object.entries(opts.builtins ?? {})) if (!resolved.has(k) && v) resolved.set(k, v);
  const unfilled = new Set<string>(malformedTokens(body));
  const filled = body.replace(INPUT_TOKEN, (m, name: string) => {
    if (resolved.has(name)) return resolved.get(name) as string;
    if (!inputs.some((i) => i.name === name)) unfilled.add(m);
    return m;
  });
  const out = resolveClusterTokens(filled, cv);
  if (opts.strict) {
    const missing = missingClusterValues(out, cv);
    if (missing.length) throw new Error(`the cluster default${missing.length === 1 ? "" : "s"} ${missing.join(", ")} ${missing.length === 1 ? "isn't" : "aren't"} set; set ${missing.length === 1 ? "it" : "them"} on the Slurm connection`);
    const left = [...unfilled];
    if (left.length) throw new Error(`the script uses ${left.join(", ")}, which no input of the recipe fills; declare ${left.length === 1 ? "it" : "them"} under inputs or remove ${left.length === 1 ? "it" : "them"}`);
  }
  return out;
}

/** The Slurm connection's cluster defaults, as the gateway stores them. */
export interface ClusterDefaultsLike {
  cache_dir?: string;
  images_dir?: string;
  log_dir?: string;
  setup?: string;
  images?: Record<string, string>;
}

/** Cluster values from the saved cluster defaults (all empty when unset). */
export function clusterValuesFrom(d: ClusterDefaultsLike | null | undefined): ClusterValues {
  return { cache: d?.cache_dir ?? "", images: d?.images_dir ?? "", logs: d?.log_dir ?? "", setup: d?.setup ?? "", image: { ...(d?.images ?? {}) } };
}
