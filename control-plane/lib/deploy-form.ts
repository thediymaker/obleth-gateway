// One deployment's settings, as the launch form edits them. The form view and
// the YAML view are two editors over this one object, and `toOverrides` turns
// it into what the launch action sends. Pure and client-safe.
import { Document, isMap, isScalar, LineCounter, parseDocument, type Node as YamlNode } from "yaml";
import type { RecipeDeployPreview } from "@/components/recipes/recipe-card";
import { accountsFor, gresCount, memToMb, partitionFits, parseTimeLimit, walltimeLabel } from "@/lib/deployments-model";
import type { ClusterResources } from "@/lib/obleth";
import { GRES_RE } from "@/lib/managed-model-schema";
import { inputDefaults, inputProblem, MODEL_NAME_RE, MODEL_NAME_RULE, type ClusterValues } from "@/lib/recipe-inputs";
import type { DeployOverrides } from "@/lib/sbatch-recipes";

export interface SlurmFields {
  partition: string;
  account: string;
  qos: string;
  time_limit: string;
  nodes: number;
  gres: string;
  cpus_per_task: string;
  mem: string;
  constraints: string;
  exclude: string;
  log_output_dir: string;
}

export interface ServingFields {
  keep_running: number;
  serve_from: number;
  stop_after_failed_launches: number;
  health_path: string;
}

export interface DeployForm {
  recipe: string;
  name: string;
  inputs: Record<string, string>;
  slurm: SlurmFields;
  env: Record<string, string>;
  serving: ServingFields;
  /** The kind of model to register, when it differs from the recipe's (an
   *  engine recipe serving an embedding model, say). */
  model_type?: string;
}

/** A dotted path into the form, e.g. "slurm.account" or "inputs.context". */
export type FieldPath = string;

/** Where a value came from, for the source tag beside each field. */
export type Source = "recipe" | "cluster" | "learned" | "changed" | "default";

/** The form as the recipe (and cluster defaults) would launch it. */
export function baselineForm(recipeId: string, p: RecipeDeployPreview, cv: ClusterValues, overrides: { name?: string; inputs?: Record<string, string>; modelType?: string } = {}): DeployForm {
  const nodes = p.nodes ?? 1;
  return {
    recipe: recipeId,
    name: overrides.name ?? p.apiModelName,
    inputs: { ...inputDefaults(p.inputs, cv, nodes), ...overrides.inputs },
    slurm: {
      partition: p.partition ?? "",
      account: p.account ?? "",
      qos: p.qos ?? "",
      time_limit: p.timeLimit ?? "",
      nodes,
      gres: p.gres ?? "",
      cpus_per_task: p.cpusPerTask ? String(p.cpusPerTask) : "",
      mem: p.mem ?? "",
      constraints: p.constraints ?? "",
      exclude: p.exclude ?? "",
      log_output_dir: p.logOutputDir || cv.logs || "",
    },
    env: { ...(p.env ?? {}) },
    serving: {
      keep_running: p.targetReplicas ?? 1,
      serve_from: 1,
      stop_after_failed_launches: p.maxJobFailures ?? 2,
      health_path: p.healthPath,
    },
    ...(overrides.modelType && overrides.modelType !== p.modelType ? { model_type: overrides.modelType } : {}),
  };
}

/** Read one value by path. */
export function getField(form: DeployForm, path: FieldPath): unknown {
  return path.split(".").reduce<unknown>((o, k) => (o && typeof o === "object" ? (o as Record<string, unknown>)[k] : undefined), form);
}

/** A copy of the form with one value replaced. */
export function setField(form: DeployForm, path: FieldPath, value: unknown): DeployForm {
  const [head, ...rest] = path.split(".");
  if (!rest.length) return { ...form, [head]: value } as DeployForm;
  const inner = (form as unknown as Record<string, Record<string, unknown>>)[head] ?? {};
  return { ...form, [head]: { ...inner, [rest.join(".")]: value } } as DeployForm;
}

/** Every leaf path whose value differs from the baseline. */
export function changedPaths(form: DeployForm, base: DeployForm): FieldPath[] {
  const out: FieldPath[] = [];
  if (form.name !== base.name) out.push("name");
  for (const section of ["inputs", "slurm", "env", "serving"] as const) {
    const a = form[section] as Record<string, unknown>;
    const b = base[section] as Record<string, unknown>;
    for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) if (String(a[k] ?? "") !== String(b[k] ?? "")) out.push(`${section}.${k}`);
  }
  return out;
}

/** What the launch action needs. Empty strings clear a recipe value. */
export function toOverrides(form: DeployForm): DeployOverrides {
  const s = form.slurm;
  const target = Math.max(0, Math.floor(form.serving.keep_running) || 0) || 1;
  return {
    api_model_name: form.name.trim(),
    ...(form.model_type ? { model_type: form.model_type } : {}),
    partition: s.partition,
    account: s.account,
    qos: s.qos,
    time_limit: s.time_limit,
    nodes: Math.max(1, Math.floor(s.nodes) || 1),
    gres: s.gres,
    cpus_per_task: s.cpus_per_task.trim() ? Number(s.cpus_per_task) : null,
    mem: s.mem,
    constraints: s.constraints,
    exclude: s.exclude,
    log_output_dir: s.log_output_dir,
    health_path: form.serving.health_path,
    target_replicas: target,
    min_replicas: Math.min(Math.max(1, Math.floor(form.serving.serve_from) || 1), target),
    max_job_failures: Math.max(0, Math.floor(form.serving.stop_after_failed_launches) || 0),
    inputs: form.inputs,
    env: Object.fromEntries(Object.entries(form.env).filter(([k]) => k.trim())),
  };
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

export interface Problem {
  path: FieldPath;
  message: string;
}

/** Everything wrong with the form that can be known before submitting. */
export function formProblems(form: DeployForm, p: RecipeDeployPreview, resources: ClusterResources, takenNames: string[]): Problem[] {
  const out: Problem[] = [];
  const name = form.name.trim();
  if (!name) out.push({ path: "name", message: "Name it: this is what clients send as model" });
  else if (!MODEL_NAME_RE.test(name)) out.push({ path: "name", message: `Names ${MODEL_NAME_RULE}, since clients send it as model and the job passes it to the server as one word` });
  else if (takenNames.includes(name)) out.push({ path: "name", message: `A model called ${name} already exists` });
  for (const input of p.inputs) {
    const value = form.inputs[input.name];
    const missing = /\{\{\s*cluster\.([a-z]+)(?:\.([A-Za-z0-9_-]+))?\s*\}\}/.exec(value ?? "");
    const why = missing ? missingDefault(input.label || input.name, missing[1], missing[2]) : inputProblem(input, value);
    if (why) out.push({ path: `inputs.${input.name}`, message: why });
  }
  const s = form.slurm;
  // A blank partition is the cluster's default partition.
  if (s.partition.trim() && resources.partitions.length && !resources.partitions.some((x) => x.name === s.partition)) out.push({ path: "slurm.partition", message: `There's no ${s.partition} partition on this cluster` });
  const takes = s.partition ? accountsFor(resources, s.partition) : null;
  // With associations, the account list is the Slurm user's own; without, it is every account on the cluster.
  const own = !!resources.associations?.length;
  if (s.account && resources.accounts.length && !resources.accounts.includes(s.account)) out.push({ path: "slurm.account", message: own ? `${s.account} isn't an account your Slurm user can charge${suggest(s.account, resources.accounts)}` : `There's no account called ${s.account} on this cluster${suggest(s.account, resources.accounts)}` });
  else if (s.account && takes && !takes.includes(s.account)) out.push({ path: "slurm.account", message: takes.length ? `${s.partition} doesn't take ${s.account}. It takes ${takes.join(", ")}.` : `${s.partition} doesn't take any of your accounts.` });
  if (s.qos && resources.qos.length && !resources.qos.includes(s.qos)) out.push({ path: "slurm.qos", message: `There's no QoS called ${s.qos} on this cluster${suggest(s.qos, resources.qos)}` });
  const minutes = parseTimeLimit(s.time_limit);
  if (s.time_limit.trim() && minutes == null) out.push({ path: "slurm.time_limit", message: "Walltime should look like D-HH:MM:SS" });
  const fit = partitionFits(resources, { gpus: gresCount(s.gres), cpus: s.cpus_per_task ? Number(s.cpus_per_task) : null, memMb: memToMb(s.mem) }).find((f) => f.name === s.partition);
  if (fit && !fit.fits) out.push({ path: "slurm.partition", message: `One replica doesn't fit a ${fit.name} node: ${fit.reason}` });
  if (fit && minutes && fit.maxMinutes && minutes > fit.maxMinutes) out.push({ path: "slurm.time_limit", message: `${walltimeLabel(minutes)} is over ${fit.name}'s ${walltimeLabel(fit.maxMinutes)} limit, so Slurm would hold the job and never start it` });
  if (!Number.isInteger(s.nodes) || s.nodes < 1) out.push({ path: "slurm.nodes", message: "Nodes must be a whole number, 1 or more" });
  if (s.cpus_per_task.trim() && !(Number(s.cpus_per_task) > 0)) out.push({ path: "slurm.cpus_per_task", message: "CPUs must be a number" });
  if (s.mem.trim() && s.mem.trim() !== "0" && memToMb(s.mem) == null) out.push({ path: "slurm.mem", message: "Memory should look like 560G, or 0 for all of it" });
  if (s.gres.trim() && !GRES_RE.test(s.gres.trim())) out.push({ path: "slurm.gres", message: "Write GPUs as Slurm gres, such as gpu:1, gpu:h100:2 or gpu:1,shard:1" });
  const logs = s.log_output_dir.trim();
  if (logs && !logs.startsWith("/") && !logs.includes("{{")) out.push({ path: "slurm.log_output_dir", message: "Job logs must be a full path starting with /. A relative folder would end up under /tmp on the node" });
  for (const k of Object.keys(form.env)) if (k && !/^[A-Za-z_][A-Za-z0-9_]*$/.test(k)) out.push({ path: `env.${k}`, message: `${k} isn't a valid variable name` });
  const sv = form.serving;
  if (!(sv.keep_running >= 1)) out.push({ path: "serving.keep_running", message: "Keep at least 1 running" });
  if (sv.serve_from > sv.keep_running) out.push({ path: "serving.serve_from", message: "Healthy when can't be more than the replicas kept running" });
  if (!sv.health_path.startsWith("/")) out.push({ path: "serving.health_path", message: "The health check is a path starting with /" });
  return out;
}

/** Where an unset cluster default lives, so the message can name it. */
const DEFAULT_NAME: Record<string, string> = {
  cache: "Cluster defaults › Weight cache",
  images: "Cluster defaults › Images folder",
  logs: "Cluster defaults › Job logs",
  setup: "Cluster defaults › Load before launch",
};
const ENGINE_NAME: Record<string, string> = { vllm: "vLLM", sglang: "SGLang", llamacpp: "llama.cpp", ollama: "Ollama" };

function missingDefault(label: string, key: string, sub?: string): string {
  const where = key === "image" && sub ? `Cluster defaults › Image per engine › ${ENGINE_NAME[sub] ?? sub}` : DEFAULT_NAME[key] ?? `the cluster default ${key}`;
  return `${label} comes from ${where}, which isn't set. Set it on the Slurm connection, or type a value here for this launch`;
}

/** ". Did you mean long?" for a near miss, else "". */
function suggest(value: string, options: string[]): string {
  const v = value.toLowerCase();
  const best = options.map((o) => ({ o, d: editDistance(v, o.toLowerCase()) })).sort((a, b) => a.d - b.d)[0];
  return best && best.d <= Math.max(2, Math.floor(value.length / 3)) ? `. Did you mean ${best.o}?` : "";
}

function editDistance(a: string, b: string): number {
  const dp = Array.from({ length: b.length + 1 }, (_, j) => j);
  for (let i = 1; i <= a.length; i++) {
    let prev = dp[0];
    dp[0] = i;
    for (let j = 1; j <= b.length; j++) {
      const tmp = dp[j];
      dp[j] = Math.min(dp[j] + 1, dp[j - 1] + 1, prev + (a[i - 1] === b[j - 1] ? 0 : 1));
      prev = tmp;
    }
  }
  return dp[b.length];
}

// ---------------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------------

const SLURM_KEYS: (keyof SlurmFields)[] = ["partition", "account", "qos", "time_limit", "nodes", "gres", "cpus_per_task", "mem", "constraints", "exclude", "log_output_dir"];
const SERVING_KEYS: (keyof ServingFields)[] = ["keep_running", "serve_from", "stop_after_failed_launches", "health_path"];
const NUMERIC = new Set(["slurm.nodes", "slurm.cpus_per_task", "serving.keep_running", "serving.serve_from", "serving.stop_after_failed_launches"]);

/** A value as YAML should show it: numbers as numbers, flags as booleans. */
function yamlValue(path: FieldPath, v: unknown): unknown {
  if (NUMERIC.has(path) && v !== "" && v != null && Number.isFinite(Number(v))) return Number(v);
  if (path.startsWith("inputs.") && (v === "true" || v === "false")) return v === "true";
  if (path.startsWith("inputs.") && typeof v === "string" && /^-?\d+(\.\d+)?$/.test(v)) return Number(v);
  return v;
}

/**
 * The form as a deploy file. With `onlyChanges`, only what differs from the
 * recipe is written (the recipe and cluster defaults supply the rest).
 * `notes` become trailing comments, e.g. {"slurm.account": "learned: …"}.
 */
export function formToYaml(form: DeployForm, base: DeployForm, { onlyChanges = true, notes = {} }: { onlyChanges?: boolean; notes?: Record<FieldPath, string> } = {}): string {
  const changed = new Set(changedPaths(form, base));
  const keep = (path: FieldPath) => !onlyChanges || changed.has(path);
  const obj: Record<string, unknown> = { recipe: form.recipe, name: form.name };
  const section = (name: "inputs" | "slurm" | "env" | "serving", keys: string[]) => {
    const vals = form[name] as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const k of keys) if (keep(`${name}.${k}`)) out[k] = yamlValue(`${name}.${k}`, vals[k] ?? "");
    if (Object.keys(out).length) obj[name] = out;
  };
  section("inputs", Object.keys(form.inputs));
  section("slurm", SLURM_KEYS);
  section("env", Object.keys({ ...base.env, ...form.env }).filter((k) => k in form.env));
  section("serving", SERVING_KEYS);
  const doc = new Document(obj);
  doc.commentBefore = onlyChanges ? " Anything left out comes from the recipe and the cluster defaults." : " Everything this deployment launches with.";
  for (const [path, text] of Object.entries(notes)) {
    const node = doc.getIn(path.split("."), true) as YamlNode | undefined;
    if (node && isScalar(node)) node.comment = ` ${text}`;
  }
  return doc.toString({ lineWidth: 0 });
}

export interface YamlResult {
  form: DeployForm | null;
  /** 1-based line numbers. */
  problems: { line: number; message: string }[];
  /** Path → line, for pointing form-level problems at the YAML. */
  lines: Record<FieldPath, number>;
}

/** Read a deploy file back onto the baseline. Unknown keys are problems, not ignored. */
export function yamlToForm(text: string, base: DeployForm, inputNames: string[]): YamlResult {
  const lc = new LineCounter();
  const doc = parseDocument(text, { lineCounter: lc, prettyErrors: false });
  const problems: YamlResult["problems"] = [];
  const lines: Record<FieldPath, number> = {};
  const lineAt = (offset: number | undefined) => (offset == null ? 1 : lc.linePos(offset).line);
  for (const e of doc.errors) problems.push({ line: lineAt(e.pos?.[0]), message: e.message.split("\n")[0] });
  if (problems.length) return { form: null, problems, lines };
  const root = doc.contents;
  if (root == null) return { form: { ...base }, problems, lines };
  if (!isMap(root)) return { form: null, problems: [{ line: 1, message: "Expected keys like recipe:, inputs:, slurm:" }], lines };

  let form: DeployForm = { ...base, inputs: { ...base.inputs }, slurm: { ...base.slurm }, env: { ...base.env }, serving: { ...base.serving } };
  const known: Record<string, string[] | null> = { recipe: null, name: null, inputs: inputNames, slurm: SLURM_KEYS, env: null, serving: SERVING_KEYS };
  for (const pair of root.items) {
    const key = isScalar(pair.key) ? String(pair.key.value) : "";
    const keyLine = lineAt((pair.key as YamlNode | null)?.range?.[0]);
    if (!(key in known)) {
      problems.push({ line: keyLine, message: `${key}: isn't a deploy setting (recipe, name, inputs, slurm, env, serving)` });
      continue;
    }
    lines[key] = keyLine;
    if (key === "recipe") {
      const v = isScalar(pair.value) ? String(pair.value.value ?? "") : "";
      if (v && v !== base.recipe) problems.push({ line: keyLine, message: `recipe: is ${base.recipe}; pick another recipe on step 1 to change it` });
      continue;
    }
    if (key === "name") {
      form = { ...form, name: isScalar(pair.value) ? String(pair.value.value ?? "") : "" };
      continue;
    }
    if (pair.value == null || (isScalar(pair.value) && pair.value.value == null)) continue;
    if (!isMap(pair.value)) {
      problems.push({ line: keyLine, message: `${key}: should hold keys, one per line` });
      continue;
    }
    const allowed = known[key];
    for (const inner of pair.value.items) {
      const k = isScalar(inner.key) ? String(inner.key.value) : "";
      const line = lineAt((inner.key as YamlNode | null)?.range?.[0]);
      const path = `${key}.${k}`;
      lines[path] = line;
      if (allowed && !allowed.includes(k)) {
        problems.push({ line, message: key === "inputs" ? `${k} isn't one of this recipe's inputs (${allowed.join(", ") || "none"})` : `${k} isn't a ${key} setting (${allowed.join(", ")})` });
        continue;
      }
      if (!isScalar(inner.value) && inner.value != null) {
        problems.push({ line, message: `${k} should be a single value` });
        continue;
      }
      const raw = inner.value == null ? "" : (inner.value as { value: unknown }).value;
      const str = raw == null ? "" : String(raw);
      if (key === "env" && str === "") {
        // An empty value removes the variable (the recipe's included).
        const { [k]: _gone, ...env } = form.env;
        form = { ...form, env };
        continue;
      }
      if (NUMERIC.has(path)) {
        if (str !== "" && !Number.isFinite(Number(str))) {
          problems.push({ line, message: `${k} should be a number` });
          continue;
        }
        form = setField(form, path, path === "slurm.cpus_per_task" ? str : Number(str || 0));
      } else {
        form = setField(form, path, str);
      }
    }
  }
  return { form: problems.length ? null : form, problems, lines };
}
