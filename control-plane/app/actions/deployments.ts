"use server";

/** Deployments and recipes. */

import { revalidatePath } from "next/cache";
import { obleth, OblethApiError } from "@/lib/obleth";
import type { ManagedModelSpec, PutManagedModel } from "@/lib/obleth";
import { requireAdmin } from "@/lib/auth/roles";
import { specFormValues, validateManagedModelForm } from "@/lib/managed-model-schema";
import { resolveRecipeById, resolveRecipeText, buildManagedFromRecipe, parseRecipe, scriptDigest, type DeployOverrides } from "@/lib/sbatch-recipes";
import { unixLineEndings } from "@/lib/sbatch-directives";
import { clusterValuesFrom, inputDefaults, type ClusterValues } from "@/lib/recipe-inputs";
import { retokenizeName, savedRecipeText, type SaveAs } from "@/lib/recipe-save";
import type { DeployForm } from "@/lib/deploy-form";
import { lookupHfModel, type HfModel } from "@/lib/hf-model";
import { actionError, type ActionResult } from "@/lib/action-result";
import { trimmed } from "@/lib/form-helpers";
import { SettingsSaveResult } from "./shared";

/**
 * Let a deployment that stopped launching try again now: clears a pause after
 * failed launches and its backoff, and the last refused submit. A gateway
 * without the retry endpoint gets its failed jobs cleared instead.
 */
export async function retryDeploymentAction(modelId: string): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    try {
      await obleth.retryManagedModel(modelId, { auditActor: session.email });
    } catch (e) {
      if (!(e instanceof OblethApiError) || (e.status !== 404 && e.status !== 405)) throw e;
      await obleth.clearLostReplicas(modelId, { auditActor: session.email });
    }
    await obleth.clearProvisionError(modelId, { auditActor: session.email }).catch(() => undefined);
  } catch (e) {
    return actionError(e);
  }
  refreshDeployments();
  return { ok: true };
}

/** Replace every running replica one at a time, each new job serving before the old one stops. */
export async function rollDeploymentAction(modelId: string): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.rollManagedModel(modelId, { auditActor: session.email });
  } catch (e) {
    if (e instanceof OblethApiError && (e.status === 404 || e.status === 405)) {
      return { ok: false, error: "This gateway can't replace replicas one at a time yet. Restart them one by one, waiting for each new one to serve." };
    }
    return actionError(e);
  }
  refreshDeployments();
  return { ok: true };
}

export async function restartReplicaAction(replicaId: string): Promise<ActionResult> {
  const session = await requireAdmin();
  try { await obleth.restartReplica(replicaId, { auditActor: session.email }); }
  catch (e) { return actionError(e); }
  revalidatePath("/deployments");
  return { ok: true };
}

export async function saveTemplateAction(
  input: { id?: string; name: string; body: string },
): Promise<ActionResult> {
  const session = await requireAdmin();
  const parsed = parseRecipe(input.id ?? "new", input.body);
  if (!parsed.valid) return { ok: false, error: parsed.error ?? "invalid recipe" };
  try {
    if (input.id) await obleth.updateRecipe(input.id, { name: input.name, body: input.body }, { auditActor: session.email });
    else await obleth.createRecipe({ name: input.name, body: input.body }, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/deployments");
  return { ok: true };
}

export async function deleteTemplateAction(id: string): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.deleteRecipe(id, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/deployments");
  return { ok: true };
}

function refreshDeployments() {
  revalidatePath("/deployments");
  revalidatePath("/models");
  revalidatePath("/fairshare");
}

function specToPut(s: ManagedModelSpec): PutManagedModel {
  return {
    enabled: s.enabled,
    partition: s.partition,
    gres: s.gres,
    nodes: s.nodes,
    constraints: s.constraints,
    exclude: s.exclude,
    account: s.account,
    qos: s.qos,
    time_limit: s.time_limit,
    cpus_per_task: s.cpus_per_task,
    mem: s.mem,
    image: s.image,
    preamble: s.preamble,
    log_output_dir: s.log_output_dir,
    launch_command: s.launch_command,
    script_body: s.script_body,
    serving_port: s.serving_port,
    health_path: s.health_path,
    min_replicas: s.min_replicas,
    target_replicas: s.target_replicas,
    max_job_failures: s.max_job_failures,
    launcher_spec: s.launcher_spec ?? null,
    // Sent only when the gateway reported them, so an older one isn't sent fields it doesn't know.
    ...(s.startup_timeout_mins !== undefined ? { startup_timeout_mins: s.startup_timeout_mins } : {}),
    ...(s.handoff_mins !== undefined ? { handoff_mins: s.handoff_mins } : {}),
  };
}

async function changeSpec(modelId: string, change: (body: PutManagedModel) => PutManagedModel | string): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    const spec = await obleth.getManagedModel(modelId);
    if (!spec) return { ok: false, error: "This model isn't launched by obleth." };
    const body = change(specToPut(spec));
    if (typeof body === "string") return { ok: false, error: body };
    await obleth.putManagedModel(modelId, body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  refreshDeployments();
  return { ok: true };
}

/** Pause (the provisioner stops its jobs) or resume a Slurm deployment. */
export async function setDeploymentEnabledAction(modelId: string, enabled: boolean): Promise<ActionResult> {
  return changeSpec(modelId, (b) => ({ ...b, enabled }));
}

/** Keep this many replicas running; the minimum follows it down. */
export async function setDeploymentReplicasAction(modelId: string, target: number): Promise<ActionResult> {
  if (!Number.isInteger(target) || target < 1 || target > 1000) return { ok: false, error: "Keep between 1 and 1000 replicas." };
  return changeSpec(modelId, (b) => ({ ...b, target_replicas: target, min_replicas: Math.min(b.min_replicas ?? 1, target) }));
}

/** Save a deployment's script, placement and service, from its page's form (`slurm_*` fields). */
export async function saveDeploymentSettingsAction(formData: FormData): Promise<SettingsSaveResult<"spec">> {
  const id = trimmed(formData.get("id"));
  const values: Record<string, string> = {};
  for (const [k, v] of formData.entries()) if (typeof v === "string" && k.startsWith("slurm_")) values[k] = v;
  const text = (k: string) => (values[`slurm_${k}`] ?? "").trim();
  const nullable = (k: string) => text(k) || null;
  const int = (k: string) => Number(text(k));
  const res = await changeSpec(id, (b) => {
    // Check only what this save changes: a value stored earlier (by a launch,
    // or under an older check) shouldn't block an unrelated edit.
    const first = Object.values(validateManagedModelForm(values, specFormValues(b)))[0];
    if (first) return first;
    // The browser sends a textarea with CRLF line endings; bash needs LF.
    const script = unixLineEndings(values.slurm_script_body ?? b.script_body ?? "");
    if (!script.trim() && !(b.launch_command ?? "").trim()) return "The script can't be empty.";
    return {
      ...b,
      script_body: script,
      partition: text("partition"),
      gres: text("gres"),
      nodes: int("nodes"),
      cpus_per_task: text("cpus_per_task") ? int("cpus_per_task") : null,
      mem: nullable("mem"),
      account: nullable("account"),
      qos: nullable("qos"),
      time_limit: nullable("time_limit"),
      constraints: nullable("constraints"),
      exclude: nullable("exclude"),
      log_output_dir: values.slurm_log_output_dir !== undefined ? text("log_output_dir").replace(/\/+$/, "") : b.log_output_dir,
      serving_port: int("serving_port"),
      health_path: text("health_path") || "/health",
      target_replicas: int("target_replicas"),
      min_replicas: int("min_replicas"),
      max_job_failures: int("max_job_failures"),
      ...(text("startup_timeout_mins") ? { startup_timeout_mins: int("startup_timeout_mins") } : {}),
      ...(text("handoff_mins") ? { handoff_mins: int("handoff_mins") } : {}),
    };
  });
  return res.ok ? { ok: true } : { ok: false, error: res.error, saved: [] };
}

/** Stop launching a model. With `deleteModel`, the model goes too; otherwise it stays in Models with no replicas. */
export async function removeDeploymentAction(modelId: string, deleteModel: boolean): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.deleteManagedModel(modelId, { auditActor: session.email });
    if (deleteModel) await obleth.deleteModel(modelId, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  refreshDeployments();
  return { ok: true };
}

/**
 * Launch a recipe as a new deployment. The API model name is the caller's to
 * choose (a recipe can be launched more than once); placement overrides win
 * over the recipe's. Returns the new model's name, for its page.
 */
/** The cluster defaults recipes fill {{cluster.*}} from; empty when unreadable. */
async function clusterValues(): Promise<ClusterValues> {
  try {
    return clusterValuesFrom((await obleth.getSlurmSettings()).cluster_defaults);
  } catch {
    return clusterValuesFrom(null);
  }
}

export async function launchRecipeAction(recipeId: string, overrides: DeployOverrides): Promise<{ ok: true; name: string } | { ok: false; error: string }> {
  const session = await requireAdmin();
  const recipe = await resolveRecipeById(recipeId);
  if (!recipe) return { ok: false, error: "That recipe no longer exists." };
  if (!recipe.valid) return { ok: false, error: recipe.error ?? "The recipe can't be read." };
  try {
    const { createBody, managedBody } = buildManagedFromRecipe(recipe, overrides, await clusterValues());
    const models = await obleth.listModels();
    if (models.some((m) => m.model_name === createBody.model_name || m.aliases?.includes(createBody.model_name) || m.variants?.some((v) => v.name === createBody.model_name))) {
      return { ok: false, error: `A model called ${createBody.model_name} already exists. Pick another name.` };
    }
    // A launched model starts staged, like one added by hand: it is made
    // active on its page once it has been tried. It can already do what the
    // recipe's flags turned on, e.g. function calling.
    const created = await obleth.createModel({ ...createBody, status: "staged" }, { auditActor: session.email });
    try {
      await obleth.putManagedModel(created.id, managedBody, { auditActor: session.email });
    } catch (e) {
      // Don't leave a model with no launch settings behind: it would block a
      // retry under the same name.
      await obleth.deleteModel(created.id, { auditActor: session.email }).catch(() => undefined);
      throw e;
    }
    refreshDeployments();
    return { ok: true, name: created.model_name };
  } catch (e) {
    return actionError(e);
  }
}

/** Save a filled-in launch as a recipe under Saved, with its values as defaults. */
export async function saveRecipeFromFormAction(recipeId: string, form: DeployForm, as: Omit<SaveAs, "basedOn" | "clusterResolved">): Promise<{ ok: true; id: string } | { ok: false; error: string }> {
  const session = await requireAdmin();
  const [text, recipe] = await Promise.all([resolveRecipeText(recipeId), resolveRecipeById(recipeId)]);
  if (!text || !recipe?.header) return { ok: false, error: "That recipe no longer exists." };
  const name = as.name.trim();
  if (!name) return { ok: false, error: "Name the recipe." };
  try {
    const cv = await clusterValues();
    const body = savedRecipeText(text, form, { ...as, name, basedOn: recipeId, clusterResolved: inputDefaults(recipe.header.inputs, cv, form.slurm.nodes) });
    const check = parseRecipe("saved", body);
    if (!check.valid) return { ok: false, error: `The saved recipe wouldn't be valid: ${check.error}` };
    const row = await obleth.createRecipe({ name, body }, { auditActor: session.email });
    revalidatePath("/deployments");
    return { ok: true, id: row.id };
  } catch (e) {
    return actionError(e);
  }
}

/** Save a running deployment's settings as a recipe. */
export async function saveRecipeFromDeploymentAction(modelId: string, name: string): Promise<{ ok: true; id: string } | { ok: false; error: string }> {
  await requireAdmin();
  try {
    const [spec, models] = await Promise.all([obleth.getManagedModel(modelId), obleth.listModels()]);
    const model = models.find((m) => m.id === modelId);
    if (!model || !spec) return { ok: false, error: "That deployment no longer exists." };
    const ls = (spec.launcher_spec ?? {}) as { recipe_id?: string; inputs?: Record<string, string>; env?: Record<string, string>; script?: string; script_digest?: string };
    if (!ls.recipe_id) return { ok: false, error: "This deployment wasn't launched from a recipe." };
    const recipe = await resolveRecipeById(ls.recipe_id);
    if (!recipe?.header) return { ok: false, error: "The recipe it was launched from no longer exists." };
    const form: DeployForm = {
      recipe: ls.recipe_id,
      name: model.model_name,
      inputs: { ...(ls.inputs ?? {}) },
      slurm: {
        partition: spec.partition,
        account: spec.account ?? "",
        qos: spec.qos ?? "",
        time_limit: spec.time_limit ?? "",
        nodes: spec.nodes || 1,
        gres: spec.gres ?? "",
        cpus_per_task: spec.cpus_per_task ? String(spec.cpus_per_task) : "",
        mem: spec.mem ?? "",
        constraints: spec.constraints ?? "",
        exclude: spec.exclude ?? "",
        log_output_dir: spec.log_output_dir ?? "",
      },
      env: { ...(ls.env ?? recipe.header.env ?? {}) },
      serving: { keep_running: spec.target_replicas, serve_from: spec.min_replicas, stop_after_failed_launches: spec.max_job_failures, health_path: spec.health_path },
    };
    const model_ = recipe.header.kind === "engine" ? ls.inputs?.model : undefined;
    // The script it runs: changed on the deployment's page after launch (the
    // submitted script no longer matches what the launch sent), edited at
    // launch, or the recipe's own.
    const live = spec.script_body ?? "";
    const changedSince = !!ls.script_digest && !!live && scriptDigest(live) !== ls.script_digest;
    const script = changedSince ? retokenizeName(live, model.model_name) : ls.script;
    return await saveRecipeFromFormAction(ls.recipe_id, form, { name, model: model_, ...(script !== undefined ? { script } : {}) });
  } catch (e) {
    return actionError(e);
  }
}

/** What a Hugging Face repo holds and which engines can run it. */
export async function lookupHfModelAction(input: string): Promise<{ ok: true; model: HfModel } | { ok: false; error: string }> {
  await requireAdmin();
  try {
    return { ok: true, model: await lookupHfModel(input) };
  } catch (e) {
    return { ok: false, error: (e as Error).message };
  }
}
