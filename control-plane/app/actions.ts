"use server";

// `updateTag` (Next 16) expires tagged Data Cache entries from a server action
// with read-your-own-writes semantics, so the post-action render refetches.
import { revalidatePath, updateTag } from "next/cache";
import { parse as parseYaml } from "yaml";
import { z } from "zod";
import { CACHE_TAGS, obleth, OblethApiError } from "@/lib/obleth";
import type {
  AutotuneReport,
  AutotuneWorkload,
  ConfigBackup,
  GuardrailsPolicy,
  ModelRoute,
  RestoreReport,
  UpdateAlertSettings,
  UpdateAutoRouterSettings,
  UpdateBoonSettings,
  UpdateSlurmSettings,
  SlurmHealthView,
} from "@/lib/obleth";
import { requireSession } from "@/lib/auth/session";
import { resolveRecipeById, buildManagedFromRecipe, parseRecipe, type DeployOverrides } from "@/lib/sbatch-recipes";

export type ActionResult = { ok: true } | { ok: false; error: string };

function actionError(e: unknown): ActionResult {
  if (e instanceof OblethApiError) return { ok: false, error: e.message };
  return {
    ok: false,
    error: e instanceof Error ? e.message : "Unexpected error",
  };
}

// ----------------------------------------------------------------------------
// Input validation
//
// Server actions are an untrusted boundary even though the dashboard is the
// only intended caller, so FormData is validated with zod before any request
// is forwarded to the obleth admin API. The schemas mirror the previous manual
// coercion (empty/blank fields fall back to defaults) and add the checks that
// were previously missing: required non-empty names, valid email/URL shapes,
// and non-negative numeric fields.
// ----------------------------------------------------------------------------

/** Trim a FormData value to a string ("" when absent). */
const trimmed = (v: unknown) => (v == null ? "" : String(v)).trim();
/** Map "" / null to undefined so zod `.default()` and `.optional()` apply. */
const blankToUndef = (v: unknown) => {
  const s = trimmed(v);
  return s === "" ? undefined : s;
};

const requiredText = (message: string) =>
  z.preprocess(trimmed, z.string().min(1, message));
const optionalText = z.preprocess(trimmed, z.string());
const checkbox = z.preprocess((v) => v === "on", z.boolean());

function normalizeModelApiName(value: unknown) {
  return trimmed(value)
    .toLowerCase()
    .replace(/[\s_]+/g, "-")
    .replace(/[^a-z0-9.-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^[.-]+|[.-]+$/g, "");
}

/** Optional positive integer (absent when the field is blank). */
const optionalPositiveInt = z.preprocess(
  blankToUndef,
  z.coerce.number().int().positive().optional(),
);
/** Optional non-negative integer. */
const optionalNonNegInt = z.preprocess(
  blankToUndef,
  z.coerce.number().int().nonnegative().optional(),
);
/** Optional non-negative number. */
const optionalNonNegNumber = z.preprocess(
  blankToUndef,
  z.coerce.number().nonnegative().optional(),
);
/** Non-negative number with a default applied when the field is blank. */
const nonNegNumber = (def: number) =>
  z.preprocess(blankToUndef, z.coerce.number().nonnegative().default(def));
/** Positive integer with a default applied when blank. */
const positiveIntWithDefault = (def: number) =>
  z.preprocess(blankToUndef, z.coerce.number().int().positive().default(def));

const tenantCreateSchema = z.object({
  name: requiredText("Tenant name is required"),
  description: optionalText,
  organization: optionalText,
  contact_email: z.preprocess(
    blankToUndef,
    z.string().email("Invalid contact email").optional(),
  ),
  status: z.preprocess(
    blankToUndef,
    z.enum(["active", "suspended", "archived"]).default("active"),
  ),
  fairshare_group: optionalText,
  weight: optionalPositiveInt,
  tokens_per_minute: optionalNonNegInt,
  max_in_flight: optionalPositiveInt,
  timezone: z.preprocess(blankToUndef, z.string().default("UTC")),
  active_from: z.preprocess(blankToUndef, z.string().datetime().optional()),
  active_until: z.preprocess(blankToUndef, z.string().datetime().optional()),
  budget_tokens: optionalNonNegInt,
  budget_cost_usd: optionalNonNegNumber,
  budget_period: z.preprocess(
    blankToUndef,
    z.enum(["lifetime", "monthly", "term"]).default("lifetime"),
  ),
});

const tenantUpdateSchema = z.object({
  id: requiredText("Missing tenant id"),
  name: requiredText("Tenant name is required"),
  description: optionalText,
  organization: optionalText,
  contact_email: z.preprocess(
    blankToUndef,
    z.string().email("Invalid contact email").optional(),
  ),
});

const modelFieldsSchema = {
  description: optionalText,
  upstream_model: optionalText,
  api_base: optionalText,
  model_type: z.preprocess(blankToUndef, z.string().default("chat")),
  input_cost_per_token: nonNegNumber(0),
  output_cost_per_token: nonNegNumber(0),
  cost_per_image: nonNegNumber(0),
  cost_per_audio_second: nonNegNumber(0),
  cost_per_character: nonNegNumber(0),
  context_window: positiveIntWithDefault(8192),
  admission_weight: positiveIntWithDefault(100),
  supports_function_calling: checkbox,
  supports_system_messages: checkbox,
  supports_response_schema: checkbox,
  supports_tool_choice: checkbox,
};

const modelApiName = z.preprocess(
  normalizeModelApiName,
  z
    .string()
    .min(1, "API model name is required")
    .regex(
      /^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/,
      "API model name can use lowercase letters, numbers, dashes, and dots.",
    ),
);

const modelCreateSchema = z.object({
  model_name: modelApiName,
  ...modelFieldsSchema,
});

const mcpCreateSchema = z.object({
  name: requiredText("Name is required"),
  upstream_url: z.preprocess(
    trimmed,
    z.string().url("A valid upstream URL is required"),
  ),
});

const keyFieldsSchema = {
  name: requiredText("Key name is required"),
  description: optionalText,
  budget_tokens: optionalNonNegInt,
  budget_cost_usd: optionalNonNegNumber,
  budget_period: z.preprocess(
    blankToUndef,
    z.enum(["lifetime", "monthly", "term"]).default("lifetime"),
  ),
  budget_started_at: z.preprocess(
    blankToUndef,
    z.string().datetime().optional(),
  ),
};

const keyCreateSchema = z.object({
  tenant_id: requiredText("Tenant is required"),
  ...keyFieldsSchema,
});

const keyUpdateSchema = z.object({
  id: requiredText("Missing key id"),
  ...keyFieldsSchema,
});

/** Return the first zod issue message for surfacing to the UI. */
function firstIssue(error: z.ZodError): string {
  return error.issues[0]?.message ?? "Invalid input";
}

const weeklyWindowSchema = z
  .array(
    z.object({
      day: z.number().int().min(0).max(6),
      start_min: z.number().int().min(0).max(1440),
      end_min: z.number().int().min(0).max(1440),
    }).refine((w) => w.end_min > w.start_min, {
      message: "Each window's end time must be after its start time.",
    }),
  )
  .default([]);

function parseWeeklyWindows(value: FormDataEntryValue | null) {
  const raw = trimmed(value);
  if (!raw) return weeklyWindowSchema.safeParse([]);
  try {
    return weeklyWindowSchema.safeParse(JSON.parse(raw));
  } catch {
    return weeklyWindowSchema.safeParse("__invalid_json__");
  }
}

export async function createTenantAction(formData: FormData): Promise<ActionResult> {
  await requireSession();
  const parsed = tenantCreateSchema.safeParse({
    name: formData.get("name"),
    description: formData.get("description"),
    organization: formData.get("organization"),
    contact_email: formData.get("contact_email"),
    status: formData.get("status"),
    fairshare_group: formData.get("fairshare_group"),
    weight: formData.get("weight"),
    tokens_per_minute: formData.get("tokens_per_minute"),
    max_in_flight: formData.get("max_in_flight"),
    timezone: formData.get("timezone"),
    active_from: formData.get("active_from"),
    active_until: formData.get("active_until"),
    budget_tokens: formData.get("budget_tokens"),
    budget_cost_usd: formData.get("budget_cost_usd"),
    budget_period: formData.get("budget_period"),
  });
  if (!parsed.success) return { ok: false, error: firstIssue(parsed.error) };
  const windows = parseWeeklyWindows(formData.get("weekly_windows"));
  if (!windows.success) return { ok: false, error: firstIssue(windows.error) };
  const allowed_models = formData
    .getAll("allowed_models")
    .map((value) => trimmed(value))
    .filter(Boolean);

  const data = parsed.data;
  if (
    data.active_from &&
    data.active_until &&
    new Date(data.active_until) <= new Date(data.active_from)
  ) {
    return { ok: false, error: "Active-until must be after active-from." };
  }

  try {
    const tenant = await obleth.createTenant({
      name: data.name,
      weight: data.weight,
      tokens_per_minute: data.tokens_per_minute ?? 0,
      max_in_flight: data.max_in_flight,
      fairshare_group: data.fairshare_group || undefined,
    });

    if (data.description || data.organization || data.contact_email) {
      await obleth.updateTenant(tenant.id, {
        name: data.name,
        description: data.description,
        organization: data.organization,
        contact_email: data.contact_email ?? "",
      });
    }

    if (data.status !== "active") {
      await obleth.setTenantStatus(tenant.id, data.status);
    }

    const hasSchedule =
      data.timezone !== "UTC" ||
      data.active_from ||
      data.active_until ||
      windows.data.length > 0;
    if (hasSchedule) {
      await obleth.setTenantSchedule(tenant.id, {
        timezone: data.timezone,
        active_from: data.active_from ?? null,
        active_until: data.active_until ?? null,
        weekly_windows: windows.data.length ? windows.data : null,
      });
    }

    const hasBudget =
      data.budget_tokens != null ||
      data.budget_cost_usd != null ||
      data.budget_period !== "lifetime";
    if (hasBudget) {
      await obleth.setTenantBudget(tenant.id, {
        budget_tokens: data.budget_tokens ?? null,
        budget_cost_usd: data.budget_cost_usd ?? null,
        budget_period: data.budget_period,
      });
    }

    if (allowed_models.length > 0) {
      await obleth.setTenantAllowlist(tenant.id, allowed_models);
    }
  } catch (e) {
    return actionError(e);
  }

  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/fairshare");
  revalidatePath("/");
  return { ok: true };
}

export async function updateTenantAction(formData: FormData) {
  await requireSession();
  const parsed = tenantUpdateSchema.safeParse({
    id: formData.get("id"),
    name: formData.get("name"),
    description: formData.get("description"),
    organization: formData.get("organization"),
    contact_email: formData.get("contact_email"),
  });
  if (!parsed.success) return;
  const { id, ...rest } = parsed.data;
  await obleth.updateTenant(id, {
    name: rest.name,
    description: rest.description,
    organization: rest.organization,
    contact_email: rest.contact_email ?? "",
  });
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/");
}

export async function setTenantStatusAction(id: string, status: string) {
  await requireSession();
  if (!id) return;
  await obleth.setTenantStatus(id, status);
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/fairshare");
  revalidatePath("/");
}

export async function setTenantScheduleAction(
  id: string,
  body: {
    timezone: string;
    active_from?: string | null;
    active_until?: string | null;
    weekly_windows?:
      | { day: number; start_min: number; end_min: number }[]
      | null;
  },
): Promise<ActionResult> {
  await requireSession();
  if (!id) return { ok: false, error: "Missing tenant id" };
  try {
    await obleth.setTenantSchedule(id, body);
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/fairshare");
  revalidatePath("/");
  return { ok: true };
}

export async function setTenantBudgetAction(
  id: string,
  body: {
    budget_tokens?: number | null;
    budget_cost_usd?: number | null;
    budget_period?: string | null;
    budget_started_at?: string | null;
  },
): Promise<ActionResult> {
  await requireSession();
  if (!id) return { ok: false, error: "Missing tenant id" };
  try {
    await obleth.setTenantBudget(id, body);
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/");
  return { ok: true };
}

export async function setTenantAllowlistAction(
  id: string,
  allowed_models: string[],
): Promise<ActionResult> {
  await requireSession();
  if (!id) return { ok: false, error: "Missing tenant id" };
  try {
    await obleth.setTenantAllowlist(id, allowed_models);
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/");
  return { ok: true };
}

export async function setTenantGuardrailsAction(
  id: string,
  policy: GuardrailsPolicy | null,
): Promise<ActionResult> {
  await requireSession();
  if (!id) return { ok: false, error: "Missing tenant id" };
  try {
    await obleth.setTenantGuardrails(id, policy);
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/");
  return { ok: true };
}

export async function deleteTenantAction(id: string) {
  await requireSession();
  if (!id) return;
  await obleth.deleteTenant(id);
  updateTag(CACHE_TAGS.tenants);
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/tenants");
  revalidatePath("/keys");
  revalidatePath("/fairshare");
  revalidatePath("/");
}

export async function setWeightAction(id: string, weight: number) {
  await requireSession();
  await obleth.setWeight(id, weight);
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/fairshare");
  revalidatePath("/");
}

export async function setQuotaAction(formData: FormData) {
  await requireSession();
  const id = String(formData.get("id"));
  const tpm = numOrUndef(formData.get("tokens_per_minute")) ?? 0;
  const mif = numOrNull(formData.get("max_in_flight"));
  if (!id || tpm < 0 || (mif !== null && mif <= 0)) return;
  await obleth.setQuota(id, tpm, mif);
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/");
}

export async function createKeyAction(
  formData: FormData,
): Promise<ActionResult & { secret?: string }> {
  await requireSession();
  const parsed = keyCreateSchema.safeParse({
    tenant_id: formData.get("tenant_id"),
    name: formData.get("name"),
    description: formData.get("description"),
    budget_tokens: formData.get("budget_tokens"),
    budget_cost_usd: formData.get("budget_cost_usd"),
    budget_period: formData.get("budget_period"),
    budget_started_at: formData.get("budget_started_at"),
  });
  if (!parsed.success) return { ok: false, error: firstIssue(parsed.error) };
  const data = parsed.data;
  const hasBudget =
    data.budget_tokens != null || data.budget_cost_usd != null;
  try {
    const created = await obleth.createKey(data.tenant_id, {
      name: data.name,
      description: data.description,
      budget_tokens: data.budget_tokens ?? null,
      budget_cost_usd: data.budget_cost_usd ?? null,
      budget_period: hasBudget ? data.budget_period : null,
      budget_started_at: hasBudget ? data.budget_started_at : null,
    });
    updateTag(CACHE_TAGS.keys);
    revalidatePath("/keys");
    return { ok: true, secret: created.secret };
  } catch (e) {
    return actionError(e);
  }
}

export async function updateKeyAction(
  formData: FormData,
): Promise<ActionResult> {
  await requireSession();
  const parsed = keyUpdateSchema.safeParse({
    id: formData.get("id"),
    name: formData.get("name"),
    description: formData.get("description"),
    budget_tokens: formData.get("budget_tokens"),
    budget_cost_usd: formData.get("budget_cost_usd"),
    budget_period: formData.get("budget_period"),
    budget_started_at: formData.get("budget_started_at"),
  });
  if (!parsed.success) return { ok: false, error: firstIssue(parsed.error) };
  const { id, ...data } = parsed.data;
  const hasBudget =
    data.budget_tokens != null || data.budget_cost_usd != null;
  try {
    await obleth.updateKey(id, {
      name: data.name,
      description: data.description,
      budget_tokens: data.budget_tokens ?? null,
      budget_cost_usd: data.budget_cost_usd ?? null,
      budget_period: hasBudget ? data.budget_period : null,
      budget_started_at: hasBudget ? data.budget_started_at : null,
    });
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
  revalidatePath("/");
  return { ok: true };
}

export async function toggleKeyAction(id: string, disabled: boolean) {
  await requireSession();
  await obleth.setKeyDisabled(id, disabled);
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
}

export async function toggleKeyTracingAction(id: string, tracing_enabled: boolean) {
  await requireSession();
  await obleth.setKeyTracing(id, tracing_enabled);
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
}

export async function toggleTenantTracingAction(id: string, tracing_enabled: boolean) {
  await requireSession();
  await obleth.setTenantTracing(id, tracing_enabled);
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
}

export async function deleteKeyAction(id: string) {
  await requireSession();
  await obleth.deleteKey(id);
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
  revalidatePath("/");
}

export async function deleteKeysAction(
  ids: string[],
): Promise<{ deleted: number; failed: number }> {
  await requireSession();
  const uniqueIds = [...new Set(ids.map((id) => String(id)).filter(Boolean))];
  const result = await deleteKeys(uniqueIds);
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
  revalidatePath("/");
  return result;
}

export async function deleteFilteredKeysAction(filters: {
  query?: string;
  tenantId?: string;
  status?: "all" | "active" | "disabled";
  budget?: "all" | "budgeted" | "unlimited";
}): Promise<{ deleted: number; failed: number; matched: number }> {
  await requireSession();
  const query = String(filters.query ?? "")
    .trim()
    .toLowerCase();
  const tenantId = String(filters.tenantId ?? "all");
  const status = filters.status ?? "all";
  const budget = filters.budget ?? "all";
  const hasFilter =
    query !== "" || tenantId !== "all" || status !== "all" || budget !== "all";
  if (!hasFilter) return { deleted: 0, failed: 0, matched: 0 };

  const [tenants, keys] = await Promise.all([
    obleth.listTenants(),
    obleth.listKeys(),
  ]);
  const tenantNames = new Map(
    tenants.map((tenant) => [tenant.id, tenant.name]),
  );
  const matched = keys.filter((key) => {
    if (tenantId !== "all" && key.tenant_id !== tenantId) return false;
    if (status === "active" && key.disabled) return false;
    if (status === "disabled" && !key.disabled) return false;
    const keyHasBudget =
      key.budget_tokens != null || key.budget_cost_usd != null;
    if (budget === "budgeted" && !keyHasBudget) return false;
    if (budget === "unlimited" && keyHasBudget) return false;
    if (!query) return true;
    const tenantName =
      tenantNames.get(key.tenant_id) ?? key.tenant_id.slice(0, 8);
    return (
      key.key_prefix.toLowerCase().includes(query) ||
      key.name.toLowerCase().includes(query) ||
      key.description.toLowerCase().includes(query) ||
      tenantName.toLowerCase().includes(query)
    );
  });

  const result = await deleteKeys(matched.map((key) => key.id));
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
  revalidatePath("/");
  return { ...result, matched: matched.length };
}

export async function setCapacityAction(max: number) {
  await requireSession();
  await obleth.setCapacity(max);
  revalidatePath("/");
  revalidatePath("/fairshare");
}

export async function createModelAction(
  formData: FormData,
): Promise<ActionResult> {
  await requireSession();
  const parsed = modelCreateSchema.safeParse({
    model_name: formData.get("model_name"),
    description: formData.get("description"),
    upstream_model: formData.get("upstream_model"),
    api_base: formData.get("api_base"),
    model_type: formData.get("model_type"),
    input_cost_per_token: formData.get("input_cost_per_token"),
    output_cost_per_token: formData.get("output_cost_per_token"),
    cost_per_image: formData.get("cost_per_image"),
    cost_per_audio_second: formData.get("cost_per_audio_second"),
    cost_per_character: formData.get("cost_per_character"),
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

  try {
    const tags = tagsFromForm(formData);
    // Slurm-provisioned models have no static upstream: the provisioner promotes
    // healthy replicas into the endpoint rotation. The gateway accepts a blank
    // api_base for these.
    const created = await obleth.createModel({
      ...parsed.data,
      api_base: isSlurm ? "" : parsed.data.api_base,
      api_key: isSlurm ? null : strOrNull(formData.get("api_key")),
      max_in_flight: numOrNull(formData.get("max_in_flight")),
      supports_vision: tags.includes("vision"),
      tags,
      boons: boonsFromForm(formData),
      tool_servers: toolServersFromForm(formData),
    });

    if (isSlurm) {
      await obleth.putManagedModel(created.id, {
        enabled: true,
        partition: trimmed(formData.get("slurm_partition")),
        gres: trimmed(formData.get("slurm_gres")),
        nodes: numOr(formData.get("slurm_nodes"), 1),
        image: trimmed(formData.get("slurm_image")),
        preamble: trimmed(formData.get("slurm_preamble")),
        log_output_dir: trimmed(formData.get("slurm_log_output_dir")),
        launch_command: trimmed(formData.get("slurm_launch_command")),
        script_body: trimmed(formData.get("slurm_script_body")),
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
      });
    }
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  return { ok: true };
}

export async function setModelCapacityAction(
  id: string,
  max_in_flight: number | null,
) {
  await requireSession();
  await obleth.setModelCapacity(id, max_in_flight);
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
}

export async function setModelCapacityModeAction(
  id: string,
  capacityMode: string,
) {
  await requireSession();
  await obleth.setModelCapacityMode(id, capacityMode);
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
}

export async function autotuneModelAction(
  id: string,
  opts?: {
    workload?: AutotuneWorkload;
    latency_headroom?: number;
    replicas?: number;
  },
): Promise<AutotuneReport> {
  await requireSession();
  // Recommend-only: drives a live probe against the upstream and returns the
  // suggested capacity. Nothing is persisted here.
  return obleth.autotuneModel(id, opts);
}

export async function applyAutotuneCapacityAction(
  id: string,
  max_in_flight: number,
) {
  await requireSession();
  await obleth.applyAutotuneCapacity(id, max_in_flight);
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
}

export async function setModelWeightAction(
  id: string,
  admission_weight: number,
) {
  await requireSession();
  await obleth.setModelWeight(id, admission_weight);
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
}

export async function deleteModelAction(id: string) {
  await requireSession();
  await obleth.deleteModel(id);
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
}

export async function setModelCacheAction(
  id: string,
  enabled: boolean,
  ttlSecs?: number,
) {
  await requireSession();
  await obleth.setModelCache(id, enabled, ttlSecs);
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
}

export async function setModelReliabilityAction(
  id: string,
  body: {
    request_timeout_secs: number | null;
    max_retries: number;
    retry_backoff_ms: number;
    endpoint_selection_mode: string;
  },
) {
  await requireSession();
  await obleth.setModelReliability(id, body);
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
}

export async function createModelEndpointAction(
  id: string,
  formData: FormData,
) {
  await requireSession();
  await obleth.createModelEndpoint(id, {
    name: String(formData.get("name") ?? "").trim(),
    api_base: String(formData.get("api_base") ?? "").trim(),
    api_key: strOrNull(formData.get("api_key")),
    priority: numOr(formData.get("priority"), 100),
    weight: numOr(formData.get("weight"), 100),
    enabled: formData.get("enabled") !== "off",
  });
  revalidatePath("/models");
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
  },
) {
  await requireSession();
  await obleth.updateModelEndpoint(id, endpointId, body);
  revalidatePath("/models");
}

export async function deleteModelEndpointAction(
  id: string,
  endpointId: string,
) {
  await requireSession();
  await obleth.deleteModelEndpoint(id, endpointId);
  revalidatePath("/models");
}

// ----------------------------------------------------------------------------
// Granular model update actions (split-tab UI)
// ----------------------------------------------------------------------------

export type ModelActionState = { ok: true } | { ok: false; error: string };

// Full replacement body for PUT /models/{id}, built from the current model so a
// partial edit re-sends every field the gateway expects. `api_key` is omitted
// on purpose: it is a write-only secret not returned by listModels, and sending
// null would clear it. Callers spread this and override only their own fields.
function toModelUpdateBody(model: ModelRoute) {
  return {
    upstream_model: model.upstream_model,
    api_base: model.api_base,
    model_type: model.model_type,
    description: model.description,
    input_cost_per_token: model.input_cost_per_token,
    output_cost_per_token: model.output_cost_per_token,
    cost_per_image: model.cost_per_image,
    cost_per_audio_second: model.cost_per_audio_second,
    cost_per_character: model.cost_per_character,
    context_window: model.context_window,
    admission_weight: model.admission_weight,
    max_in_flight: model.max_in_flight,
    supports_function_calling: model.supports_function_calling,
    supports_system_messages: model.supports_system_messages,
    supports_response_schema: model.supports_response_schema,
    supports_tool_choice: model.supports_tool_choice,
    supports_vision: model.supports_vision,
    enabled: model.enabled,
    cache_enabled: model.cache_enabled,
    cache_ttl_secs: model.cache_ttl_secs,
    tags: model.tags ?? [],
    boons: model.boons ?? [],
    tool_servers: model.tool_servers ?? [],
  };
}

// Loads the current model so a granular update can preserve untouched fields.
async function loadModel(id: string): Promise<ModelRoute | undefined> {
  const models = await obleth.listModels();
  return models.find((m) => m.id === id);
}

// Connection tab: upstream binding, model type, description, enabled, and costs.
// Preserves capabilities/tags/boons/tools/capacity by spreading the current model.
export async function updateModelConnectionAction(
  _prev: ModelActionState | null,
  formData: FormData,
): Promise<ModelActionState> {
  await requireSession();
  const id = String(formData.get("id") ?? "");
  if (!id) return { ok: false, error: "Missing model id." };
  const current = await loadModel(id);
  if (!current) return { ok: false, error: "Model not found." };

  const newKey = strOrNull(formData.get("api_key")); // only sent when non-empty
  try {
    await obleth.updateModel(id, {
      ...toModelUpdateBody(current),
      upstream_model: String(formData.get("upstream_model") ?? current.upstream_model),
      api_base: String(formData.get("api_base") ?? current.api_base),
      model_type: String(formData.get("model_type") ?? current.model_type),
      description: String(formData.get("description") ?? ""),
      enabled: formData.get("enabled") === "on",
      input_cost_per_token: numOr(formData.get("input_cost_per_token"), current.input_cost_per_token),
      output_cost_per_token: numOr(formData.get("output_cost_per_token"), current.output_cost_per_token),
      cost_per_image: numOr(formData.get("cost_per_image"), current.cost_per_image),
      cost_per_character: numOr(formData.get("cost_per_character"), current.cost_per_character),
      cost_per_audio_second: numOr(formData.get("cost_per_audio_second"), current.cost_per_audio_second),
      ...(newKey ? { api_key: newKey } : {}),
    });
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Save failed." };
  }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
  return { ok: true };
}

// Capabilities tab: native capabilities, context window, routing tags, boons,
// tools. Preserves connection/cost fields by spreading the current model.
export async function updateModelCapabilitiesAction(
  _prev: ModelActionState | null,
  formData: FormData,
): Promise<ModelActionState> {
  await requireSession();
  const id = String(formData.get("id") ?? "");
  if (!id) return { ok: false, error: "Missing model id." };
  const current = await loadModel(id);
  if (!current) return { ok: false, error: "Model not found." };

  const tags = tagsFromForm(formData);
  try {
    await obleth.updateModel(id, {
      ...toModelUpdateBody(current),
      context_window: numOr(formData.get("context_window"), current.context_window),
      supports_function_calling: formData.get("supports_function_calling") === "on",
      supports_system_messages: formData.get("supports_system_messages") === "on",
      supports_response_schema: formData.get("supports_response_schema") === "on",
      supports_tool_choice: formData.get("supports_tool_choice") === "on",
      supports_vision: tags.includes("vision"),
      tags,
      boons: boonsFromForm(formData),
      tool_servers: toolServersFromForm(formData),
    });
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Save failed." };
  }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
  return { ok: true };
}

export async function checkModelHealthAction(id: string) {
  await requireSession();
  await obleth.checkModelHealth(id);
  revalidatePath("/models");
}

export async function checkAllModelHealthAction() {
  await requireSession();
  await obleth.checkAllModelHealth();
  revalidatePath("/models");
}

export type ImportModelsResult =
  | {
      ok: true;
      created: number;
      updated: number;
      failed: number;
      errors: string[];
    }
  | { ok: false; error: string };

export interface ImportPlanItem {
  model_name: string;
  action: "create" | "update";
  upstream_model: string;
  api_base: string;
  enabled: boolean;
}

export type ImportPlanResult =
  | { ok: true; plan: ImportPlanItem[] }
  | { ok: false; error: string };

// Dry-run preview: parses the uploaded obleth models template and reports which
// routes would be created vs. updated (matched by `model_name`) without writing
// anything. The UI shows this plan and only then calls `importModelsAction`.
export async function planModelImportAction(
  text: string,
): Promise<ImportPlanResult> {
  await requireSession();
  const read = readModelInputs(text);
  if (read.error) return { ok: false, error: read.error };

  let existing: ModelRoute[];
  try {
    existing = await obleth.listModels();
  } catch (e) {
    return {
      ok: false,
      error: e instanceof Error ? e.message : "Failed to load existing models.",
    };
  }
  const names = new Set(existing.map((m) => m.model_name));

  const plan: ImportPlanItem[] = read.inputs.map((input) => ({
    model_name: input.model_name,
    action: names.has(input.model_name) ? "update" : "create",
    upstream_model: input.upstream_model,
    api_base: input.api_base,
    enabled: input.enabled ?? true,
  }));
  return { ok: true, plan };
}

// Imports model routes from an uploaded obleth models template (YAML or JSON
// with a top-level `models:` list). Existing routes are matched by `model_name`
// and updated in place; unknown names are created. Per-model failures are
// collected so a single bad entry doesn't abort the whole import.
export async function importModelsAction(
  text: string,
): Promise<ImportModelsResult> {
  await requireSession();
  const read = readModelInputs(text);
  if (read.error) return { ok: false, error: read.error };
  const inputs = read.inputs;

  let existing: ModelRoute[];
  try {
    existing = await obleth.listModels();
  } catch (e) {
    return {
      ok: false,
      error: e instanceof Error ? e.message : "Failed to load existing models.",
    };
  }
  const byName = new Map(existing.map((m) => [m.model_name, m]));

  let created = 0;
  let updated = 0;
  const errors: string[] = [];

  // One model per request is the admin API's shape, but the requests are
  // independent — run them in bounded-concurrency chunks so large imports
  // don't pay one round trip per model sequentially.
  const importOne = async (
    input: ModelImportInput,
  ): Promise<"created" | "updated"> => {
    const found = byName.get(input.model_name);
    if (found) {
      await obleth.updateModel(found.id, {
        description: input.description ?? found.description,
        upstream_model: input.upstream_model,
        api_base: input.api_base,
        api_key: input.api_key ?? undefined,
        model_type: input.model_type ?? found.model_type,
        input_cost_per_token:
          input.input_cost_per_token ?? found.input_cost_per_token,
        output_cost_per_token:
          input.output_cost_per_token ?? found.output_cost_per_token,
        cost_per_image: input.cost_per_image ?? found.cost_per_image,
        cost_per_audio_second:
          input.cost_per_audio_second ?? found.cost_per_audio_second,
        cost_per_character:
          input.cost_per_character ?? found.cost_per_character,
        context_window: input.context_window ?? found.context_window,
        admission_weight: input.admission_weight ?? found.admission_weight,
        max_in_flight:
          input.max_in_flight !== undefined
            ? input.max_in_flight
            : found.max_in_flight,
        supports_function_calling:
          input.supports_function_calling ?? found.supports_function_calling,
        supports_system_messages:
          input.supports_system_messages ?? found.supports_system_messages,
        supports_response_schema:
          input.supports_response_schema ?? found.supports_response_schema,
        supports_tool_choice:
          input.supports_tool_choice ?? found.supports_tool_choice,
        supports_vision: input.supports_vision ?? found.supports_vision,
        enabled: input.enabled ?? found.enabled,
        tags: input.tags ?? found.tags,
        boons: input.boons ?? found.boons,
      });
      return "updated";
    }
    await obleth.createModel({
      model_name: input.model_name,
      description: input.description ?? "",
      upstream_model: input.upstream_model,
      api_base: input.api_base,
      api_key: input.api_key ?? undefined,
      model_type: input.model_type ?? "chat",
      input_cost_per_token: input.input_cost_per_token ?? 0,
      output_cost_per_token: input.output_cost_per_token ?? 0,
      cost_per_image: input.cost_per_image ?? 0,
      cost_per_audio_second: input.cost_per_audio_second ?? 0,
      cost_per_character: input.cost_per_character ?? 0,
      context_window: input.context_window ?? 8192,
      admission_weight: input.admission_weight ?? 100,
      max_in_flight: input.max_in_flight ?? null,
      supports_function_calling: input.supports_function_calling ?? false,
      supports_system_messages: input.supports_system_messages ?? true,
      supports_response_schema: input.supports_response_schema ?? false,
      supports_tool_choice: input.supports_tool_choice ?? false,
      supports_vision: input.supports_vision ?? false,
      enabled: input.enabled ?? true,
      tags: input.tags ?? [],
      boons: input.boons ?? [],
    });
    return "created";
  };

  const chunkSize = 10;
  for (let i = 0; i < inputs.length; i += chunkSize) {
    const chunk = inputs.slice(i, i + chunkSize);
    const results = await Promise.allSettled(chunk.map(importOne));
    results.forEach((result, idx) => {
      if (result.status === "fulfilled") {
        if (result.value === "created") created += 1;
        else updated += 1;
      } else {
        const e = result.reason;
        const detail =
          e instanceof OblethApiError
            ? e.message
            : e instanceof Error
              ? e.message
              : "unknown error";
        errors.push(`${chunk[idx].model_name}: ${detail}`);
      }
    });
  }

  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
  return { ok: true, created, updated, failed: errors.length, errors };
}

export type RestoreBackupResult =
  | { ok: true; report: RestoreReport }
  | { ok: false; error: string };

// Restores an uploaded obleth config backup. The file is parsed and
// format-checked here, then handed to the admin API's atomic merge-restore;
// the gateway rejects it up front when its encryption key doesn't match the
// backup's. Every entity list the restore can touch is revalidated.
export async function restoreBackupAction(
  text: string,
): Promise<RestoreBackupResult> {
  await requireSession();

  let parsed: ConfigBackup;
  try {
    parsed = JSON.parse(text) as ConfigBackup;
  } catch {
    return { ok: false, error: "Not a valid JSON file." };
  }
  if (!parsed || parsed.format !== "obleth-config-backup") {
    return { ok: false, error: "Not an obleth config backup file." };
  }

  try {
    const report = await obleth.restoreBackup(parsed);
    updateTag(CACHE_TAGS.tenants);
    updateTag(CACHE_TAGS.keys);
    updateTag(CACHE_TAGS.models);
    revalidatePath("/");
    revalidatePath("/tenants");
    revalidatePath("/keys");
    revalidatePath("/models");
    revalidatePath("/mcp");
    revalidatePath("/fairshare");
    revalidatePath("/settings");
    return { ok: true, report };
  } catch (e) {
    const err = actionError(e);
    return err.ok ? { ok: false, error: "Unexpected error" } : err;
  }
}

export async function setModelHealthConfigAction(formData: FormData) {
  await requireSession();
  const id = String(formData.get("id") ?? "");
  if (!id) return;
  await obleth.setModelHealthConfig(id, {
    checks_enabled: formData.get("checks_enabled") === "on",
    alerts_enabled: formData.get("alerts_enabled") === "on",
    check_interval_secs: numOr(formData.get("check_interval_secs"), 900),
    failure_threshold: numOr(formData.get("failure_threshold"), 2),
    maintenance_until: datetimeOrNull(formData.get("maintenance_until")),
    maintenance_note: strOrNull(formData.get("maintenance_note")) ?? null,
  });
  revalidatePath("/models");
}

export async function createMcpServerAction(
  formData: FormData,
): Promise<ActionResult> {
  await requireSession();
  const parsed = mcpCreateSchema.safeParse({
    name: formData.get("name"),
    upstream_url: formData.get("upstream_url"),
  });
  if (!parsed.success) return { ok: false, error: firstIssue(parsed.error) };
  try {
    await obleth.createMcpServer({
      name: parsed.data.name,
      upstream_url: parsed.data.upstream_url,
      auth_header: strOrNull(formData.get("auth_header")),
    });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/mcp");
  return { ok: true };
}

export async function toggleMcpServerAction(
  id: string,
  upstreamUrl: string,
  enabled: boolean,
) {
  await requireSession();
  await obleth.updateMcpServer(id, { upstream_url: upstreamUrl, enabled });
  revalidatePath("/mcp");
}

export async function deleteMcpServerAction(id: string) {
  await requireSession();
  await obleth.deleteMcpServer(id);
  revalidatePath("/mcp");
}

export async function setAlertSettingsAction(
  body: UpdateAlertSettings,
): Promise<ActionResult> {
  await requireSession();
  try {
    await obleth.setAlertSettings(body);
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function setAutoRouterSettingsAction(
  body: UpdateAutoRouterSettings,
): Promise<ActionResult> {
  await requireSession();
  try {
    await obleth.setAutoRouterSettings(body);
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function setBoonSettingsAction(
  body: UpdateBoonSettings,
): Promise<ActionResult> {
  await requireSession();
  try {
    await obleth.setBoonSettings(body);
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function setCharoSettingsAction(
  enabled: boolean,
): Promise<ActionResult> {
  await requireSession();
  try {
    await obleth.setCharoSettings({ enabled });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  revalidatePath("/", "layout");
  return { ok: true };
}

export async function setSlurmSettingsAction(
  body: UpdateSlurmSettings,
): Promise<ActionResult> {
  await requireSession();
  try {
    await obleth.setSlurmSettings(body);
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  // The model-create dialog gates the Slurm option on these settings.
  revalidatePath("/models");
  return { ok: true };
}

export async function testSlurmConnectionAction(): Promise<
  (ActionResult & { health?: SlurmHealthView })
> {
  await requireSession();
  try {
    const health = await obleth.testSlurmConnection();
    return { ok: true, health };
  } catch (e) {
    return actionError(e);
  }
}

export async function setUsageRetentionAction(
  days: number,
): Promise<ActionResult> {
  await requireSession();
  if (!Number.isFinite(days) || days < 1) {
    return { ok: false, error: "Retention must be at least 1 day" };
  }
  try {
    await obleth.setUsageRetention(Math.floor(days));
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function compactUsageAction(): Promise<
  ActionResult & { partitionsDropped?: number; retentionDays?: number }
> {
  await requireSession();
  try {
    const res = await obleth.compactUsage();
    return {
      ok: true,
      partitionsDropped: res.partitions_dropped,
      retentionDays: res.retention_days,
    };
  } catch (e) {
    return actionError(e);
  }
}

export async function testAlertAction(): Promise<
  ActionResult & {
    results?: { channel: string; ok: boolean; detail: string }[];
  }
> {
  await requireSession();
  try {
    const res = await obleth.testAlert();
    return { ok: true, results: res.results };
  } catch (e) {
    return actionError(e);
  }
}

async function deleteKeys(
  ids: string[],
): Promise<{ deleted: number; failed: number }> {
  let deleted = 0;
  let failed = 0;
  const chunkSize = 25;
  for (let i = 0; i < ids.length; i += chunkSize) {
    const chunk = ids.slice(i, i + chunkSize);
    const results = await Promise.allSettled(
      chunk.map((id) => obleth.deleteKey(id)),
    );
    for (const result of results) {
      if (result.status === "fulfilled") deleted += 1;
      else failed += 1;
    }
  }
  return { deleted, failed };
}

function numOr(v: FormDataEntryValue | null, fallback: number): number {
  if (v == null || v === "") return fallback;
  const n = Number(v);
  return Number.isFinite(n) ? n : fallback;
}

// Collects checked tag checkboxes (named `tag_<name>`) from a model form into
// an array of tag names, e.g. { tag_coding: "on" } -> ["coding"].
function tagsFromForm(formData: FormData): string[] {
  const tags: string[] = [];
  for (const [key, value] of formData.entries()) {
    if (key.startsWith("tag_") && value === "on") {
      tags.push(key.slice("tag_".length));
    }
  }
  return tags;
}

// Collects checked boon checkboxes (named `boon_<name>`) from a model form into
// an array of boon names, e.g. { boon_vision: "on" } -> ["vision"].
function boonsFromForm(formData: FormData): string[] {
  const boons: string[] = [];
  for (const [key, value] of formData.entries()) {
    if (key.startsWith("boon_") && value === "on") {
      boons.push(key.slice("boon_".length));
    }
  }
  return boons;
}

// Collects checked tool-server checkboxes (named `tool_server_<name>`) from a
// model form into an array of MCP server names the model may use.
function toolServersFromForm(formData: FormData): string[] {
  const servers: string[] = [];
  for (const [key, value] of formData.entries()) {
    if (key.startsWith("tool_server_") && value === "on") {
      servers.push(key.slice("tool_server_".length));
    }
  }
  return servers;
}

function strOrNull(v: FormDataEntryValue | null): string | undefined {
  const s = String(v ?? "").trim();
  return s || undefined;
}

function numOrUndef(v: FormDataEntryValue | null): number | undefined {
  if (v == null || v === "") return undefined;
  const n = Number(v);
  return Number.isFinite(n) ? n : undefined;
}

function numOrNull(v: FormDataEntryValue | null): number | null {
  if (v == null || v === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

function datetimeOrNull(v: FormDataEntryValue | null): string | null {
  const s = String(v ?? "").trim();
  if (!s) return null;
  const date = new Date(s);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

// ----------------------------------------------------------------------------
// Model import helpers
// ----------------------------------------------------------------------------

// Normalized shape for an imported model. Only the routing essentials are
// required; everything else is optional so partial sources (e.g. LiteLLM
// configs without admission weights) fall back to sane defaults on create or
// to the existing value on update.
interface ModelImportInput {
  model_name: string;
  upstream_model: string;
  api_base: string;
  description?: string;
  api_key?: string;
  model_type?: string;
  input_cost_per_token?: number;
  output_cost_per_token?: number;
  cost_per_image?: number;
  cost_per_audio_second?: number;
  cost_per_character?: number;
  context_window?: number;
  admission_weight?: number;
  max_in_flight?: number | null;
  supports_function_calling?: boolean;
  supports_system_messages?: boolean;
  supports_response_schema?: boolean;
  supports_tool_choice?: boolean;
  supports_vision?: boolean;
  enabled?: boolean;
  tags?: string[];
  boons?: string[];
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

// Parses and normalizes an uploaded obleth models template into import inputs.
// Returns a human-readable `error` instead of throwing so callers can surface
// it directly. The template is YAML or JSON with a top-level `models:` list.
function readModelInputs(text: string): {
  inputs: ModelImportInput[];
  error?: string;
} {
  if (!text || !text.trim()) {
    return { inputs: [], error: "No file content provided." };
  }

  let parsed: unknown;
  try {
    parsed = parseModelDocument(text);
  } catch {
    return {
      inputs: [],
      error:
        "Could not parse file. Expected an obleth models YAML/JSON template.",
    };
  }

  const inputs = extractModelEntries(parsed)
    .map(toImportInput)
    .filter((m): m is ModelImportInput => m != null);

  if (inputs.length === 0) {
    return {
      inputs: [],
      error:
        "No valid models found. Use the obleth template: a top-level `models:` list where each entry has model_name, upstream_model and api_base.",
    };
  }
  return { inputs };
}

function parseModelDocument(text: string): unknown {
  const trimmed = text.trim();
  if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
    try {
      return JSON.parse(trimmed);
    } catch {
      // Fall through to YAML — YAML is a JSON superset and may still parse.
    }
  }
  return parseYaml(trimmed);
}

// Reaches the array of raw model entries in the obleth template: a top-level
// `models:` list (YAML template or JSON export), or a bare array.
function extractModelEntries(parsed: unknown): unknown[] {
  if (Array.isArray(parsed)) return parsed;
  if (isRecord(parsed) && Array.isArray(parsed.models)) return parsed.models;
  return [];
}

function toImportInput(entry: unknown): ModelImportInput | null {
  if (!isRecord(entry)) return null;
  const modelName = coerceStr(entry.model_name);
  const upstream = coerceStr(entry.upstream_model);
  const apiBase = coerceStr(entry.api_base);
  if (!modelName || !upstream || !apiBase) return null;

  return {
    model_name: modelName,
    upstream_model: upstream,
    api_base: apiBase,
    description: coerceStr(entry.description) || undefined,
    api_key: coerceStr(entry.api_key) || undefined,
    model_type: coerceStr(entry.model_type) || undefined,
    input_cost_per_token: coerceNum(entry.input_cost_per_token),
    output_cost_per_token: coerceNum(entry.output_cost_per_token),
    cost_per_image: coerceNum(entry.cost_per_image),
    cost_per_audio_second: coerceNum(entry.cost_per_audio_second),
    cost_per_character: coerceNum(entry.cost_per_character),
    context_window: coerceNum(entry.context_window),
    admission_weight: coerceNum(entry.admission_weight),
    max_in_flight:
      entry.max_in_flight == null
        ? undefined
        : (coerceNum(entry.max_in_flight) ?? null),
    supports_function_calling: coerceBool(entry.supports_function_calling),
    supports_system_messages: coerceBool(entry.supports_system_messages),
    supports_response_schema: coerceBool(entry.supports_response_schema),
    supports_tool_choice: coerceBool(entry.supports_tool_choice),
    supports_vision: coerceBool(entry.supports_vision),
    enabled: coerceBool(entry.enabled),
    tags: Array.isArray(entry.tags)
      ? entry.tags.map(coerceStr).filter(Boolean)
      : undefined,
    boons: Array.isArray(entry.boons)
      ? entry.boons.map(coerceStr).filter(Boolean)
      : undefined,
  };
}

function coerceStr(v: unknown): string {
  if (v == null) return "";
  return String(v).trim();
}

function coerceNum(v: unknown): number | undefined {
  if (v == null || v === "") return undefined;
  const n = Number(v);
  return Number.isFinite(n) ? n : undefined;
}

function coerceBool(v: unknown): boolean | undefined {
  if (typeof v === "boolean") return v;
  if (v === "true") return true;
  if (v === "false") return false;
  return undefined;
}

export async function clearLostReplicasAction(modelId: string): Promise<ActionResult> {
  await requireSession();
  try { await obleth.clearLostReplicas(modelId); }
  catch (e) { return actionError(e); }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  return { ok: true };
}

export async function saveTemplateAction(
  input: { id?: string; name: string; body: string },
): Promise<ActionResult> {
  await requireSession();
  const parsed = parseRecipe(input.id ?? "new", input.body);
  if (!parsed.valid) return { ok: false, error: parsed.error ?? "invalid recipe" };
  try {
    if (input.id) await obleth.updateRecipe(input.id, { name: input.name, body: input.body });
    else await obleth.createRecipe({ name: input.name, body: input.body });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/recipes");
  return { ok: true };
}

export async function deleteTemplateAction(id: string): Promise<ActionResult> {
  await requireSession();
  try {
    await obleth.deleteRecipe(id);
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/recipes");
  return { ok: true };
}

export async function deployRecipeAction(
  id: string,
  overrides?: DeployOverrides,
): Promise<ActionResult> {
  await requireSession();
  const recipe = await resolveRecipeById(id);
  if (!recipe) return { ok: false, error: `recipe "${id}" not found` };
  if (!recipe.valid) return { ok: false, error: recipe.error ?? "recipe is invalid" };

  try {
    const { createBody, managedBody } = buildManagedFromRecipe(recipe, overrides);
    const created = await obleth.createModel({
      model_name: createBody.model_name,
      upstream_model: createBody.upstream_model,
      api_base: createBody.api_base,
      model_type: createBody.model_type,
    });
    await obleth.putManagedModel(created.id, managedBody);
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  return { ok: true };
}
