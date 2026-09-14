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
  CharoSettingsView,
  CompressionPolicy,
  EnergyTestResult,
  GuardrailsPolicy,
  ModelImportReport,
  ModelManifest,
  ModelRoute,
  RestoreReport,
  UpdateAlertSettings,
  UpdateAutoRouterSettings,
  UpdateBoonSettings,
  UpdateEnergySettings,
  UpdateKnowledgeSettings,
  UpdateSlurmSettings,
  SlurmHealthView,
} from "@/lib/obleth";
import { requireAdmin } from "@/lib/auth/roles";
import { resolveRecipeById, buildManagedFromRecipe, parseRecipe, type DeployOverrides } from "@/lib/sbatch-recipes";
import { parseUpstreamModelList, normalizeBase, type UpstreamModel } from "@/lib/provider-import";
import { tagsInclude } from "@/lib/utils";

export type ActionResult =
  | { ok: true; warnings?: string[] }
  | { ok: false; error: string };

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
  energy_slots_per_node: z.preprocess(blankToUndef, z.coerce.number().int().nonnegative().default(0)),
  route_bias: z.preprocess(blankToUndef, z.coerce.number().min(0.1).max(3).default(1)),
  auto_eligible: checkbox,
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
  const session = await requireAdmin();
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
    }, { auditActor: session.email });

    if (data.description || data.organization || data.contact_email) {
      await obleth.updateTenant(tenant.id, {
        name: data.name,
        description: data.description,
        organization: data.organization,
        contact_email: data.contact_email ?? "",
      }, { auditActor: session.email });
    }

    if (data.status !== "active") {
      await obleth.setTenantStatus(tenant.id, data.status, { auditActor: session.email });
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
      }, { auditActor: session.email });
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
      }, { auditActor: session.email });
    }

    if (allowed_models.length > 0) {
      await obleth.setTenantAllowlist(tenant.id, allowed_models, { auditActor: session.email });
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
  const session = await requireAdmin();
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
  }, { auditActor: session.email });
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/");
}

export async function setTenantStatusAction(id: string, status: string) {
  const session = await requireAdmin();
  if (!id) return;
  await obleth.setTenantStatus(id, status, { auditActor: session.email });
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
  const session = await requireAdmin();
  if (!id) return { ok: false, error: "Missing tenant id" };
  try {
    await obleth.setTenantSchedule(id, body, { auditActor: session.email });
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
  const session = await requireAdmin();
  if (!id) return { ok: false, error: "Missing tenant id" };
  try {
    await obleth.setTenantBudget(id, body, { auditActor: session.email });
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
  const session = await requireAdmin();
  if (!id) return { ok: false, error: "Missing tenant id" };
  try {
    await obleth.setTenantAllowlist(id, allowed_models, { auditActor: session.email });
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
  const session = await requireAdmin();
  if (!id) return { ok: false, error: "Missing tenant id" };
  try {
    await obleth.setTenantGuardrails(id, policy, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/");
  return { ok: true };
}

export async function setTenantCompressionAction(
  id: string,
  policy: CompressionPolicy | null,
): Promise<ActionResult> {
  const session = await requireAdmin();
  if (!id) return { ok: false, error: "Missing tenant id" };
  try {
    await obleth.setTenantCompression(id, policy, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/");
  return { ok: true };
}

export async function deleteTenantAction(id: string) {
  const session = await requireAdmin();
  if (!id) return;
  await obleth.deleteTenant(id, { auditActor: session.email });
  updateTag(CACHE_TAGS.tenants);
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/tenants");
  revalidatePath("/keys");
  revalidatePath("/fairshare");
  revalidatePath("/");
}

export async function setWeightAction(id: string, weight: number) {
  const session = await requireAdmin();
  await obleth.setWeight(id, weight, { auditActor: session.email });
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/fairshare");
  revalidatePath("/");
}

export async function setQuotaAction(formData: FormData) {
  const session = await requireAdmin();
  const id = String(formData.get("id"));
  const tpm = numOrUndef(formData.get("tokens_per_minute")) ?? 0;
  const mif = numOrNull(formData.get("max_in_flight"));
  if (!id || tpm < 0 || (mif !== null && mif <= 0)) return;
  await obleth.setQuota(id, tpm, mif, { auditActor: session.email });
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/");
}

export async function createKeyAction(
  formData: FormData,
): Promise<ActionResult & { secret?: string }> {
  const session = await requireAdmin();
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
    const created = await obleth.createKey(
      data.tenant_id,
      {
        name: data.name,
        description: data.description,
        budget_tokens: data.budget_tokens ?? null,
        budget_cost_usd: data.budget_cost_usd ?? null,
        budget_period: hasBudget ? data.budget_period : null,
        budget_started_at: hasBudget ? data.budget_started_at : null,
      },
      { auditActor: session.email },
    );
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
  const session = await requireAdmin();
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
    await obleth.updateKey(
      id,
      {
        name: data.name,
        description: data.description,
        budget_tokens: data.budget_tokens ?? null,
        budget_cost_usd: data.budget_cost_usd ?? null,
        budget_period: hasBudget ? data.budget_period : null,
        budget_started_at: hasBudget ? data.budget_started_at : null,
      },
      { auditActor: session.email },
    );
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
  revalidatePath("/");
  return { ok: true };
}

export async function toggleKeyAction(id: string, disabled: boolean) {
  const session = await requireAdmin();
  await obleth.setKeyDisabled(id, disabled, { auditActor: session.email });
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
}

export async function toggleKeyTracingAction(id: string, tracing_enabled: boolean) {
  const session = await requireAdmin();
  await obleth.setKeyTracing(id, tracing_enabled, { auditActor: session.email });
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
}

export async function toggleTenantTracingAction(id: string, tracing_enabled: boolean) {
  const session = await requireAdmin();
  await obleth.setTenantTracing(id, tracing_enabled, { auditActor: session.email });
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
}

export async function toggleTenantSyntheticAction(id: string, synthetic: boolean) {
  const session = await requireAdmin();
  await obleth.setTenantSynthetic(id, synthetic, { auditActor: session.email });
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
}

export async function deleteKeyAction(id: string) {
  const session = await requireAdmin();
  await obleth.deleteKey(id, { auditActor: session.email });
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
  revalidatePath("/");
}

export async function deleteKeysAction(
  ids: string[],
): Promise<{ deleted: number; failed: number }> {
  const session = await requireAdmin();
  const uniqueIds = [...new Set(ids.map((id) => String(id)).filter(Boolean))];
  const result = await deleteKeys(uniqueIds, session.email);
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
  const session = await requireAdmin();
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

  const result = await deleteKeys(matched.map((key) => key.id), session.email);
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
  revalidatePath("/");
  return { ...result, matched: matched.length };
}

export async function setCapacityAction(max: number) {
  const session = await requireAdmin();
  await obleth.setCapacity(max, { auditActor: session.email });
  revalidatePath("/");
  revalidatePath("/fairshare");
}

export async function createModelAction(
  formData: FormData,
): Promise<ActionResult> {
  const session = await requireAdmin();
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
    energy_slots_per_node: formData.get("energy_slots_per_node"),
    route_bias: formData.get("route_bias"),
    auto_eligible: formData.get("auto_eligible"),
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
      supports_vision: tagsInclude(tags, "vision"),
      tags,
      boons: boonsFromForm(formData),
      tool_servers: toolServersFromForm(formData),
    }, { auditActor: session.email });

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
      }, { auditActor: session.email });
    }
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  const warnings = isSlurm
    ? undefined
    : await modelRegistrationWarnings({
        api_base: parsed.data.api_base,
        upstream_model: parsed.data.upstream_model,
        model_type: parsed.data.model_type,
      });
  return { ok: true, warnings };
}

export async function setModelCapacityAction(
  id: string,
  max_in_flight: number | null,
) {
  const session = await requireAdmin();
  await obleth.setModelCapacity(id, max_in_flight, { auditActor: session.email });
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
}

export async function setModelCapacityModeAction(
  id: string,
  capacityMode: string,
) {
  const session = await requireAdmin();
  await obleth.setModelCapacityMode(id, capacityMode, { auditActor: session.email });
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
  const session = await requireAdmin();
  // Recommend-only: drives a live probe against the upstream and returns the
  // suggested capacity. Nothing is persisted here.
  return obleth.autotuneModel(id, opts, { auditActor: session.email });
}

export async function applyAutotuneCapacityAction(
  id: string,
  max_in_flight: number,
) {
  const session = await requireAdmin();
  await obleth.applyAutotuneCapacity(id, max_in_flight, { auditActor: session.email });
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
}

export async function setModelWeightAction(
  id: string,
  admission_weight: number,
) {
  const session = await requireAdmin();
  await obleth.setModelWeight(id, admission_weight, { auditActor: session.email });
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
}

export async function deleteModelAction(id: string) {
  const session = await requireAdmin();
  await obleth.deleteModel(id, { auditActor: session.email });
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
}

export async function setModelCacheAction(
  id: string,
  enabled: boolean,
  ttlSecs?: number,
) {
  const session = await requireAdmin();
  await obleth.setModelCache(id, enabled, ttlSecs, { auditActor: session.email });
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
    debug_diagnostics: boolean;
  },
) {
  const session = await requireAdmin();
  await obleth.setModelReliability(id, body, { auditActor: session.email });
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
}

export async function createModelEndpointAction(
  id: string,
  formData: FormData,
) {
  const session = await requireAdmin();
  await obleth.createModelEndpoint(id, {
    name: String(formData.get("name") ?? "").trim(),
    api_base: String(formData.get("api_base") ?? "").trim(),
    api_key: strOrNull(formData.get("api_key")),
    priority: numOr(formData.get("priority"), 100),
    weight: numOr(formData.get("weight"), 100),
    enabled: formData.get("enabled") !== "off",
  }, { auditActor: session.email });
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
  const session = await requireAdmin();
  await obleth.updateModelEndpoint(id, endpointId, body, { auditActor: session.email });
  revalidatePath("/models");
}

export async function deleteModelEndpointAction(
  id: string,
  endpointId: string,
) {
  const session = await requireAdmin();
  await obleth.deleteModelEndpoint(id, endpointId, { auditActor: session.email });
  revalidatePath("/models");
}

// ----------------------------------------------------------------------------
// Granular model update actions (split-tab UI)
// ----------------------------------------------------------------------------

export type ModelActionState =
  | { ok: true; warnings?: string[] }
  | { ok: false; error: string };

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
    energy_slots_per_node: model.energy_slots_per_node,
    route_bias: model.route_bias,
    auto_eligible: model.auto_eligible,
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
  const session = await requireAdmin();
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
      energy_slots_per_node: numOr(formData.get("energy_slots_per_node"), current.energy_slots_per_node),
      route_bias: numOr(formData.get("route_bias"), current.route_bias),
      auto_eligible: formData.get("auto_eligible") === "on",
      ...(newKey ? { api_key: newKey } : {}),
    }, { auditActor: session.email });
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Save failed." };
  }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
  const warnings = await modelRegistrationWarnings({
    api_base: String(formData.get("api_base") ?? current.api_base),
    upstream_model: String(formData.get("upstream_model") ?? current.upstream_model),
    model_type: String(formData.get("model_type") ?? current.model_type),
  });
  return { ok: true, warnings };
}

// Capabilities tab: native capabilities, context window, routing tags, boons,
// tools. Preserves connection/cost fields by spreading the current model.
export async function updateModelCapabilitiesAction(
  _prev: ModelActionState | null,
  formData: FormData,
): Promise<ModelActionState> {
  const session = await requireAdmin();
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
      supports_vision: tagsInclude(tags, "vision"),
      tags,
      boons: boonsFromForm(formData),
      tool_servers: toolServersFromForm(formData),
    }, { auditActor: session.email });
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Save failed." };
  }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/fairshare");
  return { ok: true };
}

export async function checkModelHealthAction(id: string) {
  await requireAdmin();
  await obleth.checkModelHealth(id);
  revalidatePath("/models");
}

export async function checkAllModelHealthAction() {
  await requireAdmin();
  await obleth.checkAllModelHealth();
  revalidatePath("/models");
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

  try {
    const parsed = new URL(base);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return { ok: false, error: "Provider URL must be http or https." };
    }
  } catch {
    return { ok: false, error: "Enter a valid http(s) URL." };
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 15_000);
  try {
    const res = await fetch(`${base}/models`, {
      headers: {
        Accept: "application/json",
        ...(input.apiKey ? { Authorization: `Bearer ${input.apiKey}` } : {}),
      },
      signal: controller.signal,
    });
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
    if (e instanceof Error && e.name === "AbortError") {
      return { ok: false, error: "Provider did not respond within 15s." };
    }
    return { ok: false, error: e instanceof Error ? e.message : "Could not reach the provider." };
  } finally {
    clearTimeout(timer);
  }
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
  const session = await requireAdmin();

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
    const report = await obleth.restoreBackup(parsed, { auditActor: session.email });
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

export type ApplyManifestResult =
  | { ok: true; report: ModelImportReport }
  | { ok: false; error: string };

// Parses an uploaded model file into a manifest the gateway will accept.
//
// Two shapes are allowed. A current manifest carries `format: "obleth-models"`.
// A bare `models:` list — the shape the older models template used — is
// accepted too and wrapped, so template files written before the manifest
// existed keep working. YAML and JSON both parse; YAML is a JSON superset, so
// one parser covers the JSON case when a strict JSON parse fails.
function readModelManifest(
  text: string,
): { manifest: ModelManifest } | { error: string } {
  if (!text.trim()) return { error: "No file content provided." };

  let parsed: unknown;
  const trimmed = text.trim();
  try {
    parsed =
      trimmed.startsWith("{") || trimmed.startsWith("[")
        ? JSON.parse(trimmed)
        : parseYaml(trimmed);
  } catch {
    try {
      parsed = parseYaml(trimmed);
    } catch {
      return { error: "Could not parse the file as JSON or YAML." };
    }
  }

  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { error: "Expected a document with a top-level `models` list." };
  }
  const doc = parsed as Record<string, unknown>;

  const isManifest = doc.format === "obleth-models";
  if (doc.format != null && !isManifest) {
    return {
      error: `Not an obleth model file (format ${JSON.stringify(doc.format)}).`,
    };
  }
  if (!Array.isArray(doc.models)) {
    return { error: "Expected a document with a top-level `models` list." };
  }
  if (doc.models.length === 0) {
    return { error: "The file lists no models." };
  }
  const bad = doc.models.findIndex(
    (m) =>
      !m ||
      typeof m !== "object" ||
      typeof (m as Record<string, unknown>).model_name !== "string" ||
      !(m as Record<string, unknown>).model_name,
  );
  if (bad >= 0) {
    return { error: `Model at position ${bad + 1} has no model_name.` };
  }

  return {
    manifest: {
      format: "obleth-models",
      // Only a real manifest's version is meaningful; a bare `models:` list
      // may carry an unrelated `version` from the older template shape.
      version: isManifest && typeof doc.version === "number" ? doc.version : 1,
      models: doc.models as ModelManifest["models"],
    },
  };
}

// Applies an uploaded model manifest. Call with dryRun to validate and preview
// the per-model diff without writing; call again with dryRun false to commit.
// The gateway validates the whole file before touching anything, so a rejected
// manifest leaves the registry exactly as it was.
export async function applyModelManifestAction(
  text: string,
  dryRun: boolean,
): Promise<ApplyManifestResult> {
  const session = await requireAdmin();

  const read = readModelManifest(text);
  if ("error" in read) return { ok: false, error: read.error };

  try {
    const report = await obleth.importModels(read.manifest, {
      dryRun,
      auditActor: session.email,
    });
    // A dry run writes nothing, so there is nothing to revalidate.
    if (!dryRun) {
      updateTag(CACHE_TAGS.models);
      revalidatePath("/models");
      revalidatePath("/");
    }
    return { ok: true, report };
  } catch (e) {
    const err = actionError(e);
    return err.ok ? { ok: false, error: "Unexpected error" } : err;
  }
}

export async function setModelHealthConfigAction(formData: FormData) {
  const session = await requireAdmin();
  const id = String(formData.get("id") ?? "");
  if (!id) return;
  await obleth.setModelHealthConfig(id, {
    checks_enabled: formData.get("checks_enabled") === "on",
    alerts_enabled: formData.get("alerts_enabled") === "on",
    check_interval_secs: numOr(formData.get("check_interval_secs"), 900),
    failure_threshold: numOr(formData.get("failure_threshold"), 2),
    maintenance_until: datetimeOrNull(formData.get("maintenance_until")),
    maintenance_note: strOrNull(formData.get("maintenance_note")) ?? null,
  }, { auditActor: session.email });
  revalidatePath("/models");
}

export async function createMcpServerAction(
  formData: FormData,
): Promise<ActionResult> {
  const session = await requireAdmin();
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
    }, { auditActor: session.email });
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
  const session = await requireAdmin();
  await obleth.updateMcpServer(id, { upstream_url: upstreamUrl, enabled }, { auditActor: session.email });
  revalidatePath("/mcp");
}

export async function deleteMcpServerAction(id: string) {
  const session = await requireAdmin();
  await obleth.deleteMcpServer(id, { auditActor: session.email });
  revalidatePath("/mcp");
}

export async function setAlertSettingsAction(
  body: UpdateAlertSettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.setAlertSettings(body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

const autoRouterUpdateSchema = z.object({
  classifier_enabled: z.boolean().optional(),
  classifier_model: z.string().optional(),
  classifier_timeout_ms: z.number().optional(),
  capacity_weight: z.number().min(0).max(1).optional(),
  cost_weight: z.number().min(0).max(1).optional(),
  tag_weight: z.number().min(0).max(1).optional(),
  default_soft_cap: z.number().int().min(1).optional(),
  temperature: z.number().min(0).max(2).optional(),
  difficulty_enabled: z.boolean().optional(),
  tier_source: z.enum(["hybrid", "derived", "declared"]).optional(),
});

export async function setAutoRouterSettingsAction(
  body: UpdateAutoRouterSettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  const parsed = autoRouterUpdateSchema.safeParse(body);
  if (!parsed.success) return { ok: false, error: firstIssue(parsed.error) };
  try {
    await obleth.setAutoRouterSettings(parsed.data, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function setBoonSettingsAction(
  body: UpdateBoonSettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.setBoonSettings(body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function setKnowledgeSettingsAction(
  body: UpdateKnowledgeSettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.updateKnowledgeSettings(body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function setEnergySettingsAction(
  body: UpdateEnergySettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.setEnergySettings(body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function testEnergyQueryAction(
  prometheusUrl: string,
  powerQuery: string,
): Promise<ActionResult & { data?: EnergyTestResult }> {
  await requireAdmin();
  try {
    const data = await obleth.testEnergyQuery(prometheusUrl, powerQuery);
    return { ok: true, data };
  } catch (e) {
    return actionError(e);
  }
}

export async function setCharoSettingsAction(
  next: CharoSettingsView,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.setCharoSettings(next, { auditActor: session.email });
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
  const session = await requireAdmin();
  try {
    await obleth.setSlurmSettings(body, { auditActor: session.email });
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
  await requireAdmin();
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
  const session = await requireAdmin();
  if (!Number.isFinite(days) || days < 1) {
    return { ok: false, error: "Retention must be at least 1 day" };
  }
  try {
    await obleth.setUsageRetention(Math.floor(days), { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function compactUsageAction(): Promise<
  ActionResult & { partitionsDropped?: number; retentionDays?: number }
> {
  const session = await requireAdmin();
  try {
    const res = await obleth.compactUsage({ auditActor: session.email });
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
  await requireAdmin();
  try {
    const res = await obleth.testAlert();
    return { ok: true, results: res.results };
  } catch (e) {
    return actionError(e);
  }
}

async function deleteKeys(
  ids: string[],
  auditActor: string,
): Promise<{ deleted: number; failed: number }> {
  let deleted = 0;
  let failed = 0;
  const chunkSize = 25;
  for (let i = 0; i < ids.length; i += chunkSize) {
    const chunk = ids.slice(i, i + chunkSize);
    const results = await Promise.allSettled(
      chunk.map((id) => obleth.deleteKey(id, { auditActor })),
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

// Collects checked tag checkboxes (named `tag_<name>`) from a model form,
// folding in each tag's paired strength level (`tag_level_<name>`, 1-3) into
// a `base:level` string, e.g. { tag_coding: "on", tag_level_coding: "3" } ->
// ["coding:3"]. Level 1 serializes as the bare tag name so an untouched
// model's stored tags round-trip byte-identical; this mirrors the gateway's
// `parse_tag_level` convention on the Rust side.
function tagsFromForm(formData: FormData): string[] {
  const tags: string[] = [];
  for (const [key, value] of formData.entries()) {
    if (!key.startsWith("tag_") || key.startsWith("tag_level_") || value !== "on") continue;
    const base = key.slice("tag_".length);
    const level = clampTagLevel(formData.get(`tag_level_${base}`));
    tags.push(level > 1 ? `${base}:${level}` : base);
  }
  return tags;
}

function clampTagLevel(raw: FormDataEntryValue | null): number {
  const n = Number(raw);
  if (!Number.isFinite(n)) return 1;
  return Math.min(3, Math.max(1, Math.trunc(n)));
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

export async function clearLostReplicasAction(modelId: string): Promise<ActionResult> {
  const session = await requireAdmin();
  try { await obleth.clearLostReplicas(modelId, { auditActor: session.email }); }
  catch (e) { return actionError(e); }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  return { ok: true };
}

export async function restartReplicaAction(replicaId: string): Promise<ActionResult> {
  const session = await requireAdmin();
  try { await obleth.restartReplica(replicaId, { auditActor: session.email }); }
  catch (e) { return actionError(e); }
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
  revalidatePath("/recipes");
  return { ok: true };
}

export async function deleteTemplateAction(id: string): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.deleteRecipe(id, { auditActor: session.email });
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
  const session = await requireAdmin();
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
    }, { auditActor: session.email });
    await obleth.putManagedModel(created.id, managedBody, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  return { ok: true };
}
