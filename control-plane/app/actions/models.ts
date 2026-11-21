"use server";

/** Models: create, endpoints, capacity, the model page's settings form, families, activation and bulk changes. */

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { obleth } from "@/lib/obleth";
import type { AutotuneReport, AutotuneWorkload, ModelFamilyWrite, ModelReasoning, ModelRoute, SetModelStatus } from "@/lib/obleth";
import { requireAdmin } from "@/lib/auth/roles";
import { unixLineEndings } from "@/lib/sbatch-directives";
import { parseUpstreamModelList, normalizeBase, type UpstreamModel } from "@/lib/provider-import";
import { blockedHostReason } from "@/lib/ssrf";
import { familyWrite, MAX_FAMILY_DESCRIPTION_CHARS, reasoningProblem } from "@/lib/models-model";
import { tagsInclude } from "@/lib/utils";
import { actionError, type ActionResult } from "@/lib/action-result";
import { blankToUndef, numOr, numOrNull, optionalText, trimmed } from "@/lib/form-helpers";
import { aliasesFromForm, boonsFromForm, datetimeOrNull, deleteAndRevalidate, firstIssue, loadModel, modelCreateSchema, modelUpdateKeys, namesFromForm, sessionAffinityFromForm, strOrNull, tagsFromForm, toolServersFromForm, upstreamHeadersFromForm, variantsFromForm } from "./shared";

export type CreateModelResult =
  | {
      ok: true;
      warnings?: string[];
      model: { id: string; name: string };
      /** Whether the first health check passed and turned the route on. */
      enabled: boolean;
      /** The first check's message when it did not pass. */
      check: string | null;
    }
  | { ok: false; error: string };

/**
 * Create a model. A model that connects to an endpoint is created switched
 * off and health-checked straight away; it comes on only if that check
 * passes, so a mistyped URL or name never takes traffic. A Slurm model is
 * created on: its provisioner brings replicas up and health-gates them.
 */
export async function createModelAction(
  formData: FormData,
): Promise<CreateModelResult> {
  const session = await requireAdmin();
  const parsed = modelCreateSchema.safeParse({
    model_name: formData.get("model_name"),
    description: formData.get("description"),
    upstream_model: formData.get("upstream_model"),
    api_base: formData.get("api_base"),
    model_type: formData.get("model_type"),
    quantization: formData.get("quantization"),
    input_cost_per_token: formData.get("input_cost_per_token"),
    output_cost_per_token: formData.get("output_cost_per_token"),
    cost_per_image: formData.get("cost_per_image"),
    cost_per_audio_second: formData.get("cost_per_audio_second"),
    cost_per_character: formData.get("cost_per_character"),
    cost_per_video: formData.get("cost_per_video"),
    energy_slots_per_node: formData.get("energy_slots_per_node"),
    route_bias: formData.get("route_bias"),
    auto_eligible: formData.get("auto_eligible"),
    draft_model: formData.get("draft_model"),
    verify_api_base: formData.get("verify_api_base"),
    verify_upstream_model: formData.get("verify_upstream_model"),
    context_window: formData.get("context_window"),
    admission_weight: formData.get("admission_weight"),
    supports_function_calling: formData.get("supports_function_calling"),
    supports_system_messages: formData.get("supports_system_messages"),
    supports_response_schema: formData.get("supports_response_schema"),
    supports_tool_choice: formData.get("supports_tool_choice"),
  });
  if (!parsed.success) return { ok: false, error: firstIssue(parsed.error) };

  const endpointMode = trimmed(formData.get("endpoint_mode")) || "static";
  const isSlurm = endpointMode === "slurm";
  if (isSlurm && !trimmed(formData.get("slurm_partition"))) {
    return { ok: false, error: "Slurm partition is required" };
  }
  if (
    isSlurm &&
    !trimmed(formData.get("slurm_launch_command")) &&
    !trimmed(formData.get("slurm_script_body"))
  ) {
    return { ok: false, error: "Slurm launch command or job script is required" };
  }

  let created: ModelRoute;
  try {
    const tags = tagsFromForm(formData);
    // Slurm-provisioned models have no static upstream: the provisioner promotes
    // healthy replicas into the endpoint rotation. The gateway accepts a blank
    // api_base for these.
    created = await obleth.createModel({
      ...parsed.data,
      api_base: isSlurm ? "" : parsed.data.api_base,
      api_key: isSlurm ? null : strOrNull(formData.get("api_key")),
      max_in_flight: numOrNull(formData.get("max_in_flight")),
      supports_vision: tagsInclude(tags, "vision"),
      tags,
      aliases: aliasesFromForm(formData),
      ...upstreamHeadersFromForm(formData),
      boons: boonsFromForm(formData),
      tool_servers: toolServersFromForm(formData),
      ...(isSlurm ? {} : { enabled: false }),
      ...(formData.get("staged") === "on" ? { status: "staged" as const } : {}),
    }, { auditActor: session.email });

    if (isSlurm) {
      await obleth.putManagedModel(created.id, {
        enabled: true,
        partition: trimmed(formData.get("slurm_partition")),
        gres: trimmed(formData.get("slurm_gres")),
        nodes: numOr(formData.get("slurm_nodes"), 1),
        image: trimmed(formData.get("slurm_image")),
        preamble: unixLineEndings(trimmed(formData.get("slurm_preamble"))),
        log_output_dir: trimmed(formData.get("slurm_log_output_dir")),
        launch_command: unixLineEndings(trimmed(formData.get("slurm_launch_command"))),
        script_body: unixLineEndings(trimmed(formData.get("slurm_script_body"))),
        cpus_per_task: numOrNull(formData.get("slurm_cpus_per_task")),
        mem: strOrNull(formData.get("slurm_mem")) ?? null,
        serving_port: numOr(formData.get("slurm_serving_port"), 8000),
        health_path: trimmed(formData.get("slurm_health_path")) || "/health",
        target_replicas: numOr(formData.get("slurm_target_replicas"), 2),
        max_job_failures: numOr(formData.get("slurm_max_job_failures"), 0),
        launcher_spec: (() => {
          const raw = trimmed(formData.get("slurm_launcher_spec"));
          if (!raw) return null;
          try { return JSON.parse(raw) as Record<string, unknown>; }
          catch { return null; }
        })(),
        account: strOrNull(formData.get("slurm_account")) ?? null,
        qos: strOrNull(formData.get("slurm_qos")) ?? null,
        time_limit: strOrNull(formData.get("slurm_time_limit")) ?? null,
        constraints: strOrNull(formData.get("slurm_constraints")) ?? null,
        exclude: strOrNull(formData.get("slurm_exclude")) ?? null,
      }, { auditActor: session.email });
    }
  } catch (e) {
    return actionError(e);
  }
  let enabled = isSlurm;
  let check: string | null = null;
  if (!isSlurm) {
    try {
      const activation = await obleth.activateModel(created.id, { auditActor: session.email });
      enabled = activation.enabled;
      if (!enabled) check = activation.detail.summary.last_message ?? `Health check came back ${activation.detail.summary.status}.`;
    } catch (e) {
      check = `The first health check could not run: ${actionError(e).error}`;
    }
  }
  revalidatePath("/models");
  const warnings = isSlurm
    ? undefined
    : await modelRegistrationWarnings({
        api_base: parsed.data.api_base,
        upstream_model: parsed.data.upstream_model,
        model_type: parsed.data.model_type,
      });
  return { ok: true, warnings, model: { id: created.id, name: created.model_name }, enabled, check };
}

/** A Kubernetes Service name (RFC 1035 label), as the gateway checks it. */
const SERVICE_NAME = /^[a-z]([-a-z0-9]{0,61}[a-z0-9])?$/;

const capacityDiscoveryFields = z.object({
  capacity_source: z.enum(["endpoints", "kubernetes"], {
    message: "Pick a capacity source",
  }),
  capacity_namespace: optionalText,
  capacity_service: z.preprocess(
    trimmed,
    z
      .string()
      .refine(
        (v) => v === "" || SERVICE_NAME.test(v),
        "Service name must be lowercase letters, digits and '-', starting with a letter (at most 63 characters)",
      ),
  ),
  per_replica_max_in_flight: z.preprocess(
    blankToUndef,
    z.coerce
      .number()
      .int("Per-replica concurrency must be a whole number")
      .min(1, "Per-replica concurrency must be at least 1")
      .max(100_000, "Per-replica concurrency must be at most 100000")
      .optional(),
  ),
  capacity_headroom: z.preprocess(
    blankToUndef,
    z.coerce
      .number()
      .gt(0, "Headroom must be above 0")
      .max(10, "Headroom must be at most 10")
      .default(1),
  ),
});

// The kubernetes source reads only replica counts, so the concurrency of one
// replica has to come from here. The endpoints source can take it from each
// endpoint instead; the gateway checks that against the endpoints.
const capacityDiscoverySchema = capacityDiscoveryFields.refine(
  (d) => d.capacity_source !== "kubernetes" || d.per_replica_max_in_flight != null,
  {
    message:
      "Per-replica concurrency is required for the kubernetes source (e.g. your server's max concurrent sequences, such as vLLM --max-num-seqs)",
    path: ["per_replica_max_in_flight"],
  },
);

export async function autotuneModelAction(
  id: string,
  opts?: {
    workload?: AutotuneWorkload;
    latency_headroom?: number;
    replicas?: number;
  },
): Promise<{ ok: true; report: AutotuneReport } | { ok: false; error: string }> {
  const session = await requireAdmin();
  // Recommend-only: drives a live probe against the upstream and returns the
  // suggested capacity. Nothing is persisted here.
  try {
    return { ok: true, report: await obleth.autotuneModel(id, opts, { auditActor: session.email }) };
  } catch (e) {
    return actionError(e);
  }
}

export async function applyAutotuneCapacityAction(
  id: string,
  max_in_flight: number,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.applyAutotuneCapacity(id, max_in_flight, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/models");
  revalidatePath("/fairshare");
  return { ok: true };
}

export async function deleteModelAction(id: string): Promise<ActionResult> {
  const session = await requireAdmin();
  return deleteAndRevalidate(
    () => obleth.deleteModel(id, { auditActor: session.email }),
    () => {
      revalidatePath("/models");
    },
  );
}

export async function createModelEndpointAction(
  id: string,
  formData: FormData,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.createModelEndpoint(id, {
      name: String(formData.get("name") ?? "").trim(),
      api_base: String(formData.get("api_base") ?? "").trim(),
      api_key: strOrNull(formData.get("api_key")),
      priority: numOr(formData.get("priority"), 100),
      weight: numOr(formData.get("weight"), 100),
      enabled: formData.get("enabled") !== "off",
      max_in_flight: numOrNull(formData.get("max_in_flight")),
    }, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/models");
  return { ok: true };
}

export async function updateModelEndpointAction(
  id: string,
  endpointId: string,
  body: {
    name: string;
    api_base: string;
    api_key?: string | null;
    priority?: number;
    weight?: number;
    enabled?: boolean;
    max_in_flight?: number | null;
  },
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.updateModelEndpoint(id, endpointId, body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/models");
  return { ok: true };
}

export async function deleteModelEndpointAction(
  id: string,
  endpointId: string,
): Promise<ActionResult> {
  const session = await requireAdmin();
  return deleteAndRevalidate(
    () => obleth.deleteModelEndpoint(id, endpointId, { auditActor: session.email }),
    () => revalidatePath("/models"),
  );
}

// Advisory post-save validation: does the upstream actually list this model?
// Never blocks or fails a save — validation trouble just means no warnings.
async function modelRegistrationWarnings(body: {
  api_base: string;
  upstream_model: string;
  model_type?: string | null;
}): Promise<string[] | undefined> {
  // Provisioned-only models (blank api_base) are verified per endpoint.
  if (!body.api_base.trim()) return undefined;
  try {
    const result = await obleth.validateModel(body);
    return result.warnings.length > 0 ? result.warnings : undefined;
  } catch {
    return undefined;
  }
}

/** The parts of a model's settings form, each saved through its own endpoint. */
export type ModelSettingsSection = "model" | "cache" | "reliability" | "health" | "capacity" | "knowledge" | "listing";

const MODEL_SETTINGS_SECTIONS: readonly ModelSettingsSection[] = ["model", "cache", "reliability", "health", "capacity", "knowledge", "listing"];

const SECTION_LABELS: Record<ModelSettingsSection, string> = {
  model: "Model settings",
  cache: "Response cache",
  reliability: "Delivery",
  health: "Health checks",
  capacity: "Capacity",
  knowledge: "Knowledge collections",
  listing: "Listing",
};

export type ModelSettingsResult =
  | { ok: true; warnings?: string[] }
  | { ok: false; error: string; saved: ModelSettingsSection[] };

/**
 * The PUT body for a model from its settings form, starting from the stored
 * model so nothing the form leaves out is lost. Text and number fields apply
 * when present. Checkbox groups submit nothing when every box is clear, so
 * each group carries a marker (`has_routing`, `has_tags`, `has_capabilities`)
 * and applies only when its marker is there. The route switch is not part of
 * the form (the page header owns it), so `enabled` always carries through.
 */
function modelSettingsBody(formData: FormData, current: ModelRoute) {
  const has = (name: string) => formData.has(name);
  const text = (name: string, fallback: string) => (has(name) ? String(formData.get(name) ?? "") : fallback);
  const on = (name: string) => formData.get(name) === "on";
  const newKey = strOrNull(formData.get("api_key"));
  const tags = has("has_tags") ? tagsFromForm(formData) : null;
  const capabilities = has("has_capabilities");
  return {
    ...modelUpdateKeys(current),
    description: text("description", current.description),
    model_type: text("model_type", current.model_type),
    quantization: text("quantization", current.quantization),
    ...(has("aliases") ? { aliases: aliasesFromForm(formData) } : {}),
    ...(has("rejected_request_fields") ? { rejected_request_fields: namesFromForm(formData, "rejected_request_fields") } : {}),
    ...(has("session_affinity_mode") ? { session_affinity: sessionAffinityFromForm(formData) } : {}),
    ...(has("variants") ? { variants: variantsFromForm(formData) } : {}),
    upstream_model: text("upstream_model", current.upstream_model),
    api_base: text("api_base", current.api_base),
    ...(newKey ? { api_key: newKey } : {}),
    ...upstreamHeadersFromForm(formData),
    input_cost_per_token: numOr(formData.get("input_cost_per_token"), current.input_cost_per_token),
    output_cost_per_token: numOr(formData.get("output_cost_per_token"), current.output_cost_per_token),
    cost_per_image: numOr(formData.get("cost_per_image"), current.cost_per_image),
    cost_per_character: numOr(formData.get("cost_per_character"), current.cost_per_character),
    cost_per_video: numOr(formData.get("cost_per_video"), current.cost_per_video),
    cost_per_audio_second: numOr(formData.get("cost_per_audio_second"), current.cost_per_audio_second),
    energy_slots_per_node: numOr(formData.get("energy_slots_per_node"), current.energy_slots_per_node),
    context_window: numOr(formData.get("context_window"), current.context_window),
    admission_weight: numOr(formData.get("admission_weight"), current.admission_weight),
    route_bias: numOr(formData.get("route_bias"), current.route_bias),
    auto_eligible: has("has_routing") ? on("auto_eligible") : current.auto_eligible,
    ...(tags ? { tags, supports_vision: tagsInclude(tags, "vision") } : {}),
    ...(capabilities
      ? {
          supports_function_calling: on("supports_function_calling"),
          supports_system_messages: on("supports_system_messages"),
          supports_response_schema: on("supports_response_schema"),
          supports_tool_choice: on("supports_tool_choice"),
          boons: boonsFromForm(formData),
          tool_servers: toolServersFromForm(formData),
        }
      : {}),
    draft_model: text("draft_model", current.draft_model ?? ""),
    verify_api_base: text("verify_api_base", current.verify_api_base ?? ""),
    verify_upstream_model: text("verify_upstream_model", current.verify_upstream_model ?? ""),
    enabled: current.enabled,
  };
}

/**
 * Save a model's settings form. `formData` carries `id` and `sections` (the
 * changed sections, comma-separated) alongside the fields. Sections save in
 * a fixed order and stop at the first refusal, which names what was already
 * saved so the page can say so.
 */
export async function saveModelSettingsAction(formData: FormData): Promise<ModelSettingsResult> {
  const session = await requireAdmin();
  const audit = { auditActor: session.email };
  const id = String(formData.get("id") ?? "");
  const wanted = new Set(String(formData.get("sections") ?? "").split(",").map((s) => s.trim()));
  const sections = MODEL_SETTINGS_SECTIONS.filter((s) => wanted.has(s));
  const saved: ModelSettingsSection[] = [];
  if (!id) return { ok: false, error: "Missing model id.", saved };
  if (sections.length === 0) return { ok: true };
  const current = await loadModel(id);
  if (!current) return { ok: false, error: "Model not found.", saved };

  let warnings: string[] | undefined;
  const refresh = () => {
    revalidatePath("/models");
    revalidatePath("/fairshare");
  };
  for (const section of sections) {
    try {
      if (section === "model") {
        const body = modelSettingsBody(formData, current);
        await obleth.updateModel(id, body, audit);
        const moved =
          body.api_base !== current.api_base ||
          body.upstream_model !== current.upstream_model ||
          body.model_type !== current.model_type;
        if (moved) warnings = await modelRegistrationWarnings(body);
      } else if (section === "cache") {
        await obleth.setModelCache(id, formData.get("cache_enabled") === "on", numOr(formData.get("cache_ttl_secs"), 300), audit);
      } else if (section === "reliability") {
        const rawTimeout = trimmed(formData.get("request_timeout_secs"));
        await obleth.setModelReliability(id, {
          request_timeout_secs: rawTimeout === "" ? null : Number(rawTimeout),
          max_retries: numOr(formData.get("max_retries"), current.max_retries),
          retry_backoff_ms: numOr(formData.get("retry_backoff_ms"), current.retry_backoff_ms),
          endpoint_selection_mode: trimmed(formData.get("endpoint_selection_mode")) || current.endpoint_selection_mode,
          debug_diagnostics: formData.get("debug_diagnostics") === "on",
        }, audit);
      } else if (section === "health") {
        await obleth.setModelHealthConfig(id, {
          checks_enabled: formData.get("checks_enabled") === "on",
          alerts_enabled: formData.get("alerts_enabled") === "on",
          check_interval_secs: numOr(formData.get("check_interval_secs"), 900),
          failure_threshold: numOr(formData.get("failure_threshold"), 2),
          maintenance_until: datetimeOrNull(formData.get("maintenance_until")),
          maintenance_note: strOrNull(formData.get("maintenance_note")) ?? null,
        }, audit);
      } else if (section === "capacity") {
        const error = await saveCapacity(id, formData, current, audit);
        if (error) {
          refresh();
          return { ok: false, error: `${SECTION_LABELS.capacity}: ${error}`, saved };
        }
      } else if (section === "listing") {
        await obleth.setModelListing(id, formData.get("listed") === "on", audit);
      } else if (section === "knowledge" && formData.has("knowledge_loaded")) {
        await obleth.setModelCollections(id, formData.getAll("knowledge_collection").map(String), audit);
      }
      saved.push(section);
    } catch (e) {
      refresh();
      return { ok: false, error: `${SECTION_LABELS[section]}: ${actionError(e).error}`, saved };
    }
  }
  refresh();
  return { ok: true, warnings };
}

/**
 * The Capacity section: the static cap (also the fallback while discovery
 * has no answer), then the mode, with the discovered mode's settings.
 * Returns a message when the form fails validation.
 */
async function saveCapacity(
  id: string,
  formData: FormData,
  current: ModelRoute,
  audit: { auditActor: string },
): Promise<string | null> {
  const cap = numOrNull(formData.get("max_in_flight"));
  const nextCap = cap == null ? null : Math.max(1, Math.round(cap));
  if (nextCap !== current.max_in_flight) await obleth.setModelCapacity(id, nextCap, audit);
  const mode = trimmed(formData.get("capacity_mode")) || current.capacity_mode;
  if (mode === "discovered") {
    const parsed = capacityDiscoverySchema.safeParse({
      capacity_source: formData.get("capacity_source"),
      capacity_namespace: formData.get("capacity_namespace"),
      capacity_service: formData.get("capacity_service"),
      per_replica_max_in_flight: formData.get("per_replica_max_in_flight"),
      capacity_headroom: formData.get("capacity_headroom"),
    });
    if (!parsed.success) return firstIssue(parsed.error);
    const data = parsed.data;
    await obleth.setModelCapacityMode(id, "discovered", {
      capacity_source: data.capacity_source,
      capacity_namespace: data.capacity_namespace || null,
      capacity_service: data.capacity_service || null,
      per_replica_max_in_flight: data.per_replica_max_in_flight ?? null,
      capacity_headroom: data.capacity_headroom,
    }, audit);
  } else if (mode !== current.capacity_mode) {
    await obleth.setModelCapacityMode(id, mode, undefined, audit);
  }
  return null;
}

/** The page header's Serving switch. */
export async function setModelEnabledAction(id: string, enabled: boolean): Promise<ActionResult> {
  const session = await requireAdmin();
  const current = await loadModel(id);
  if (!current) return { ok: false, error: "Model not found." };
  try {
    await obleth.updateModel(id, { ...modelUpdateKeys(current), enabled }, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/models");
  revalidatePath("/");
  return { ok: true };
}

/**
 * Set a model's lifecycle from the Lifecycle section: active, deprecated or
 * retired, with the replacement, retirement date, note and redirect. The
 * gateway validates the replacement and replaces the lifecycle whole.
 */
export async function setModelStatusAction(id: string, body: SetModelStatus): Promise<ActionResult> {
  const session = await requireAdmin();
  const note = body.note?.trim() ?? "";
  if (note.length > 500) return { ok: false, error: "Keep the note to 500 characters; callers see it with every refusal." };
  if (body.redirect && !body.replacement) return { ok: false, error: "Pick a replacement to answer in its place." };
  try {
    await obleth.setModelStatus(
      id,
      body.status === "active" ? { status: "active" } : { ...body, note: note || null, replacement: body.replacement || null },
      { auditActor: session.email },
    );
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/models");
  revalidatePath("/");
  return { ok: true };
}

/**
 * Set how a model takes reasoning effort, from the Reasoning section: pass
 * through, translate the four levels, or remove the caller's effort. The map
 * is written whole; the gateway checks it again and answers 400 with a
 * sentence when it can't take it.
 */
export async function setModelReasoningAction(id: string, body: ModelReasoning): Promise<ActionResult> {
  const session = await requireAdmin();
  const problem = reasoningProblem(body);
  if (problem) return { ok: false, error: problem };
  try {
    await obleth.setModelReasoning(id, body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/models");
  return { ok: true };
}

/**
 * Create a model family (no `id`) or replace one whole. The gateway checks
 * the name against every model and family and that each level names a chat
 * model, and stores the members by their own names.
 */
export async function saveModelFamilyAction(id: string | null, body: ModelFamilyWrite): Promise<ActionResult> {
  const session = await requireAdmin();
  const family = familyWrite({ ...body, description: body.description ?? "" });
  if (!family.name) return { ok: false, error: "Give the family a name. Callers send it as the model." };
  if ((family.description ?? "").length > MAX_FAMILY_DESCRIPTION_CHARS) return { ok: false, error: `Keep the description to ${MAX_FAMILY_DESCRIPTION_CHARS} characters.` };
  try {
    if (id) await obleth.updateModelFamily(id, family, { auditActor: session.email });
    else await obleth.createModelFamily(family, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/models");
  return { ok: true };
}

export async function deleteModelFamilyAction(id: string): Promise<ActionResult> {
  const session = await requireAdmin();
  return deleteAndRevalidate(
    () => obleth.deleteModelFamily(id, { auditActor: session.email }),
    () => revalidatePath("/models"),
  );
}

export type ActivateModelResult =
  | { ok: true; enabled: boolean; activated: boolean; status: string; message: string | null }
  | { ok: false; error: string };

/**
 * Health-check a model and, when it is switched off, turn it on if the check
 * comes back healthy. New models are created off and come on this way.
 */
export async function activateModelAction(id: string): Promise<ActivateModelResult> {
  const session = await requireAdmin();
  try {
    const result = await obleth.activateModel(id, { auditActor: session.email });
    revalidatePath("/models");
    return {
      ok: true,
      enabled: result.enabled,
      activated: result.activated,
      status: result.detail.summary.status,
      message: result.detail.summary.last_message,
    };
  } catch (e) {
    return actionError(e);
  }
}

export interface BulkModelResult {
  done: number;
  failed: { name: string; error: string }[];
}

/** Run `task` over the chosen models, a few at a time, collecting failures by name. */
async function eachModel(
  ids: string[],
  task: (model: ModelRoute) => Promise<void>,
): Promise<BulkModelResult> {
  const wanted = new Set(ids);
  const models = (await obleth.listModels()).filter((m) => wanted.has(m.id));
  const failed: BulkModelResult["failed"] = [];
  let done = 0;
  for (let i = 0; i < models.length; i += 4) {
    await Promise.all(
      models.slice(i, i + 4).map(async (model) => {
        try {
          await task(model);
          done += 1;
        } catch (e) {
          failed.push({ name: model.model_name, error: actionError(e).error });
        }
      }),
    );
  }
  revalidatePath("/models");
  revalidatePath("/");
  return { done, failed };
}

export async function setModelsEnabledAction(ids: string[], enabled: boolean): Promise<BulkModelResult> {
  const session = await requireAdmin();
  return eachModel(ids, async (model) => {
    if (model.enabled === enabled) return;
    await obleth.updateModel(model.id, { ...modelUpdateKeys(model), enabled }, { auditActor: session.email });
  });
}

/**
 * First health check for models that were just imported switched off: each
 * that passes comes on, the rest stay off with the check's reason.
 */
export async function activateModelsAction(names: string[]): Promise<{ on: string[]; off: { name: string; error: string }[] }> {
  const session = await requireAdmin();
  const wanted = new Set(names);
  const models = (await obleth.listModels()).filter((m) => wanted.has(m.model_name) && !m.enabled);
  const on: string[] = [];
  const off: { name: string; error: string }[] = [];
  for (let i = 0; i < models.length; i += 4) {
    await Promise.all(
      models.slice(i, i + 4).map(async (model) => {
        try {
          const result = await obleth.activateModel(model.id, { auditActor: session.email });
          if (result.enabled) on.push(model.model_name);
          else off.push({ name: model.model_name, error: result.detail.summary.last_message ?? `Health check came back ${result.detail.summary.status}.` });
        } catch (e) {
          off.push({ name: model.model_name, error: actionError(e).error });
        }
      }),
    );
  }
  revalidatePath("/models");
  return { on, off };
}

export async function checkModelsHealthAction(ids: string[]): Promise<BulkModelResult> {
  await requireAdmin();
  return eachModel(ids, async (model) => {
    await obleth.checkModelHealth(model.id);
  });
}

export async function deleteModelsAction(ids: string[]): Promise<BulkModelResult> {
  const session = await requireAdmin();
  return eachModel(ids, async (model) => {
    await obleth.deleteModel(model.id, { auditActor: session.email });
  });
}

export async function checkModelHealthAction(id: string): Promise<ActionResult> {
  await requireAdmin();
  try {
    await obleth.checkModelHealth(id);
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/models");
  return { ok: true };
}

export type UpstreamModelsResult =
  | { ok: true; base: string; models: UpstreamModel[] }
  | { ok: false; error: string };

// Server-side discovery of a provider's catalog. Fetches GET {base}/models with
// an optional bearer key (the key never leaves the server). Returns the parsed,
// deduped, sorted model list, or a message tailored to the failure class.
export async function listUpstreamModelsAction(input: {
  apiBase: string;
  apiKey?: string;
}): Promise<UpstreamModelsResult> {
  await requireAdmin();
  const base = normalizeBase(input.apiBase ?? "");
  if (!base) return { ok: false, error: "Enter the provider API base URL." };

  let parsed: URL;
  try {
    parsed = new URL(base);
  } catch {
    return { ok: false, error: "Enter a valid http(s) URL." };
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return { ok: false, error: "Provider URL must be http or https." };
  }
  const blocked = await blockedHostReason(parsed.hostname);
  if (blocked) return { ok: false, error: blocked };

  try {
    // Redirects are not followed: the target was vetted above, a redirect's
    // destination (e.g. a metadata endpoint) would not be.
    const res = await fetch(`${base}/models`, {
      headers: {
        Accept: "application/json",
        ...(input.apiKey ? { Authorization: `Bearer ${input.apiKey}` } : {}),
      },
      redirect: "manual",
      signal: AbortSignal.timeout(5_000),
    });
    if (res.status >= 300 && res.status < 400) {
      return { ok: false, error: `Provider redirected (HTTP ${res.status}). Enter the final API base URL.` };
    }
    if (res.status === 401 || res.status === 403) {
      return { ok: false, error: `Provider rejected the API key (HTTP ${res.status}).` };
    }
    if (res.status === 404) {
      return {
        ok: false,
        error: "No /models endpoint here (HTTP 404). Check whether the base URL should include or drop /v1.",
      };
    }
    if (!res.ok) {
      return { ok: false, error: `Provider returned HTTP ${res.status}.` };
    }
    let json: unknown;
    try {
      json = await res.json();
    } catch {
      return { ok: false, error: "Provider response was not valid JSON." };
    }
    const models = parseUpstreamModelList(json);
    if (models.length === 0) {
      return { ok: false, error: "Provider returned no models." };
    }
    return { ok: true, base, models };
  } catch (e) {
    if (e instanceof Error && (e.name === "TimeoutError" || e.name === "AbortError")) {
      return { ok: false, error: "Provider did not respond within 5s." };
    }
    return { ok: false, error: e instanceof Error ? e.message : "Could not reach the provider." };
  }
}
