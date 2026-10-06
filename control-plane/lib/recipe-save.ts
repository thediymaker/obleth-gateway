// "Save as recipe": a recipe file with the values a deployment used written in
// as its defaults, so it launches the same way next time. The source recipe's
// header is edited in place (comments and order survive) and its script is kept.
import { isMap, isSeq, parseDocument, type YAMLMap } from "yaml";
import type { DeployForm } from "@/lib/deploy-form";
import { splitFrontmatter } from "@/lib/recipe-frontmatter";

export interface SaveAs {
  /** The saved recipe's name, e.g. "Nemotron 3 Ultra · 4 nodes". */
  name: string;
  /** The recipe it came from (a library id or a saved recipe's id). */
  basedOn: string;
  /** For a recipe made from an engine: the model it now serves. */
  model?: string;
  weightsGb?: number | null;
  /** Input values as the cluster defaults resolve them, to tell "left as the
   *  cluster default" apart from a value someone typed. */
  clusterResolved?: Record<string, string>;
}

/** "Nemotron 3 Ultra on Grace Hopper · gh200 · 4 nodes". */
export function savedName(recipeName: string, form: DeployForm): string {
  const bits = [form.slurm.partition, form.slurm.nodes > 1 ? `${form.slurm.nodes} nodes` : null].filter(Boolean);
  return [recipeName.replace(/\s·.*$/, ""), ...bits].join(" · ");
}

/** Write numbers and flags unquoted, as a person would. */
function plain(v: string): string | number | boolean {
  if (v === "true" || v === "false") return v === "true";
  return /^-?\d+(\.\d+)?$/.test(v) ? Number(v) : v;
}

function setOrDelete(map: YAMLMap<string, unknown>, key: string, value: string | number | null | undefined) {
  if (value === undefined || value === null || value === "") map.delete(key);
  else map.set(key, value);
}

/** The source recipe with the form's values as its defaults. Throws on a malformed source. */
export function savedRecipeText(source: string, form: DeployForm, as: SaveAs): string {
  const split = splitFrontmatter(source);
  if (!split) throw new Error("The recipe it came from can't be read.");
  const doc = parseDocument(split.header);
  if (!isMap(doc.contents)) throw new Error("The recipe it came from has no header.");
  const h = doc.contents as unknown as YAMLMap<string, unknown>;

  h.set("name", as.name);
  h.set("based_on", as.basedOn);
  h.set("api_model_name", form.name.trim());
  if (as.model) {
    h.set("kind", "model");
    h.set("model", as.model);
    setOrDelete(h, "weights_gb", as.weightsGb ?? null);
  }
  const s = form.slurm;
  setOrDelete(h, "partition", s.partition);
  setOrDelete(h, "account", s.account);
  setOrDelete(h, "qos", s.qos);
  setOrDelete(h, "time_limit", s.time_limit);
  setOrDelete(h, "gres", s.gres);
  setOrDelete(h, "cpus_per_task", s.cpus_per_task ? Number(s.cpus_per_task) : null);
  setOrDelete(h, "mem", s.mem);
  setOrDelete(h, "nodes", s.nodes);
  setOrDelete(h, "constraints", s.constraints);
  setOrDelete(h, "exclude", s.exclude);
  setOrDelete(h, "log_output_dir", s.log_output_dir);
  h.set("target_replicas", form.serving.keep_running);
  h.set("min_replicas", form.serving.serve_from);
  h.set("max_job_failures", form.serving.stop_after_failed_launches);
  h.set("health_path", form.serving.health_path);
  // Edit env in place so the recipe's comments on its variables survive.
  const env = Object.fromEntries(Object.entries(form.env).filter(([k]) => k.trim()));
  const envNode = h.get("env", true);
  if (!Object.keys(env).length) h.delete("env");
  else if (isMap(envNode)) {
    const m = envNode as unknown as YAMLMap<string, unknown>;
    for (const pair of [...m.items]) {
      const k = String((pair.key as { value?: unknown })?.value ?? pair.key);
      if (!(k in env)) m.delete(k);
    }
    for (const [k, v] of Object.entries(env)) if (String(m.get(k) ?? "") !== v) m.set(k, v);
  } else h.set("env", env);

  // Each input's (or older variable's) default becomes the value used. A value
  // that is still the cluster default is left as the {{cluster.*}} reference,
  // so the saved recipe keeps following the cluster's setting.
  for (const key of ["inputs", "variables"]) {
    const list = h.get(key, true);
    if (!isSeq(list)) continue;
    for (const node of list.items) {
      if (!isMap(node)) continue;
      const item = node as unknown as YAMLMap<string, unknown>;
      const name = String(item.get("name") ?? "");
      if (!(name in form.inputs)) continue;
      const was = item.get("default");
      const value = form.inputs[name];
      if (was != null && String(was).includes("{{cluster.") && as.clusterResolved?.[name] === value) continue;
      if (value === "") item.delete("default");
      else item.set("default", plain(value));
      // Keep per-node defaults, with this node count's entry set to the value used.
      const byNodes = item.get("by_nodes", true);
      if (isMap(byNodes) && value !== "") (byNodes as unknown as YAMLMap<string, unknown>).set(String(form.slurm.nodes), plain(value));
    }
  }
  return `---\n${doc.toString({ lineWidth: 0 })}---\n${split.body}`;
}
