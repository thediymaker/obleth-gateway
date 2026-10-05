"use server";

// `updateTag` (Next 16) expires tagged Data Cache entries from a server action
// with read-your-own-writes semantics, so the post-action render refetches.
import { revalidatePath, updateTag } from "next/cache";
import { parse as parseYaml } from "yaml";
import { z } from "zod";
import { CACHE_TAGS, obleth, OblethApiError } from "@/lib/obleth";
import type {
  ApiKey,
  ManagedModelSpec,
  PutManagedModel,
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
  SetModelStatus,
  ModelVariantInput,
  RestoreReport,
  ResyncReport,
  UpdateAlertSettings,
  UpdateAutoRouterSettings,
  UpdateBoonSettings,
  SpeculationCategoryGate,
  UpdateEnergySettings,
  UpdateKnowledgeSettings,
  UpdateSlurmSettings,
  SlurmHealthView,
} from "@/lib/obleth";
import { requireAdmin } from "@/lib/auth/roles";
import { validateManagedModelForm } from "@/lib/managed-model-schema";
import { parseUpstreamHeaders } from "@/lib/upstream-headers";
import { resolveRecipeById, resolveRecipeText, buildManagedFromRecipe, parseRecipe, type DeployOverrides } from "@/lib/sbatch-recipes";
import { unixLineEndings } from "@/lib/sbatch-directives";
import { clusterValuesFrom, inputDefaults, type ClusterValues } from "@/lib/recipe-inputs";
import { savedRecipeText, type SaveAs } from "@/lib/recipe-save";
import type { DeployForm } from "@/lib/deploy-form";
import { lookupHfModel, type HfModel } from "@/lib/hf-model";
import { parseUpstreamModelList, normalizeBase, type UpstreamModel } from "@/lib/provider-import";
import { blockedHostReason } from "@/lib/ssrf";
import { tagsInclude } from "@/lib/utils";

export type ActionResult =
  | { ok: true; warnings?: string[] }
  | { ok: false; error: string };

function actionError(e: unknown): { ok: false; error: string } {
  if (e instanceof OblethApiError) return { ok: false, error: e.message };
  return {
    ok: false,
    error: e instanceof Error ? e.message : "Unexpected error",
  };
}

/**
 * Run a delete and revalidate whether or not it succeeded: the Management API
 * answers 502 when the row is gone from Postgres but the data-plane cache
 * eviction failed, so the list must refresh and the operator must see that
 * message (it names the reconcile endpoint).
 */
async function deleteAndRevalidate(
  run: () => Promise<unknown>,
  revalidate: () => void,
): Promise<ActionResult> {
  try {
    await run();
    return { ok: true };
  } catch (e) {
    return actionError(e);
  } finally {
    revalidate();
  }
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
  // Free-form here; the gateway owns the vocabulary and rejects anything
  // outside it, so the dashboard does not need a second copy of the list to
  // fall out of date.
  quantization: z.preprocess(blankToUndef, z.string().default("unknown")),
  input_cost_per_token: nonNegNumber(0),
  output_cost_per_token: nonNegNumber(0),
  cost_per_image: nonNegNumber(0),
  cost_per_audio_second: nonNegNumber(0),
  cost_per_character: nonNegNumber(0),
  cost_per_video: nonNegNumber(0),
  energy_slots_per_node: z.preprocess(blankToUndef, z.coerce.number().int().nonnegative().default(0)),
  route_bias: z.preprocess(blankToUndef, z.coerce.number().min(0.1).max(3).default(1)),
  auto_eligible: checkbox,
  draft_model: optionalText,
  verify_api_base: optionalText,
  verify_upstream_model: optionalText,
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

// Mirrors the gateway: the name is the `/mcp/<name>` path segment and the key models grant it by.
const MCP_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const MCP_NAME_HINT = "Use letters, digits, '.', '_' or '-', starting with a letter or digit (up to 64).";

const mcpCreateSchema = z.object({
  name: z.preprocess(trimmed, z.string().min(1, "Name is required").regex(MCP_NAME_RE, MCP_NAME_HINT)),
  upstream_url: z.preprocess(
    trimmed,
    z.string().url("A valid upstream URL is required"),
  ),
});

const keyFieldsSchema = {
  name: requiredText("Key name is required"),
  description: optionalText,
  weight: z.preprocess(blankToUndef, z.coerce.number().int().min(1, "Weight must be at least 1").default(100)),
  max_in_flight: optionalPositiveInt,
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

export async function setTenantStatusAction(id: string, status: string): Promise<ActionResult> {
  const session = await requireAdmin();
  if (!id) return { ok: false, error: "Missing tenant id" };
  if (!["active", "suspended", "archived"].includes(status)) return { ok: false, error: "Unknown status." };
  try {
    await obleth.setTenantStatus(id, status, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.tenants);
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/tenants");
  revalidatePath("/fairshare");
  revalidatePath("/");
  return { ok: true };
}

export async function deleteTenantAction(id: string): Promise<ActionResult> {
  const session = await requireAdmin();
  if (!id) return { ok: false, error: "Missing tenant id" };
  return deleteAndRevalidate(
    () => obleth.deleteTenant(id, { auditActor: session.email }),
    () => {
      updateTag(CACHE_TAGS.tenants);
      updateTag(CACHE_TAGS.keys);
      revalidatePath("/tenants");
      revalidatePath("/keys");
      revalidatePath("/fairshare");
      revalidatePath("/");
    },
  );
}

export async function setWeightAction(id: string, weight: number) {
  const session = await requireAdmin();
  await obleth.setWeight(id, weight, { auditActor: session.email });
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/tenants");
  revalidatePath("/fairshare");
  revalidatePath("/");
}

/** A fairshare group's weight: its share of a full pool against the other active groups. */
export async function setGroupWeightAction(name: string, weight: number): Promise<ActionResult> {
  const session = await requireAdmin();
  if (!name || !Number.isInteger(weight) || weight < 1) return { ok: false, error: "Weight must be a whole number of at least 1." };
  try {
    await obleth.setFairshareGroupWeight(name, weight, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/fairshare");
  revalidatePath("/fairshare/groups");
  return { ok: true };
}

/**
 * A tenant's per-model in-flight cap (null clears it). The quota endpoint
 * sets tokens per minute alongside, so the tenant's current value is read and
 * carried over rather than reset.
 */
export async function setTenantMaxInFlightAction(id: string, maxInFlight: number | null): Promise<ActionResult> {
  const session = await requireAdmin();
  if (maxInFlight !== null && (!Number.isInteger(maxInFlight) || maxInFlight < 1)) return { ok: false, error: "The cap must be a whole number of at least 1, or empty for no limit." };
  try {
    const tenant = (await obleth.listTenants()).find((t) => t.id === id);
    if (!tenant) return { ok: false, error: "Tenant not found." };
    await obleth.setQuota(id, tenant.tokens_per_minute, maxInFlight, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.tenants);
  revalidatePath("/fairshare");
  revalidatePath("/tenants");
  return { ok: true };
}

export async function createKeyAction(
  formData: FormData,
): Promise<ActionResult & { secret?: string }> {
  const session = await requireAdmin();
  const parsed = keyCreateSchema.safeParse({
    tenant_id: formData.get("tenant_id"),
    name: formData.get("name"),
    description: formData.get("description"),
    weight: formData.get("weight"),
    max_in_flight: formData.get("max_in_flight"),
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
        weight: data.weight,
        max_in_flight: data.max_in_flight ?? null,
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
    weight: formData.get("weight"),
    max_in_flight: formData.get("max_in_flight"),
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
        weight: data.weight,
        max_in_flight: data.max_in_flight ?? null,
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

// ----------------------------------------------------------------------------
// A tenant's page, and the key panel
//
// The tenant page submits every setting as one form and saves with one
// button; each changed section goes through the endpoint that owns it, in a
// fixed order, and a refusal says which sections were already saved.
// ----------------------------------------------------------------------------

export type TenantSettingsSection =
  | "profile"
  | "group"
  | "weight"
  | "quota"
  | "tracing"
  | "synthetic"
  | "schedule"
  | "budget"
  | "allowlist"
  | "guardrails"
  | "compression";

const TENANT_SECTIONS: readonly TenantSettingsSection[] = ["profile", "group", "weight", "quota", "tracing", "synthetic", "schedule", "budget", "allowlist", "guardrails", "compression"];

const TENANT_SECTION_LABELS: Record<TenantSettingsSection, string> = {
  profile: "Profile",
  group: "Fairshare group",
  weight: "Weight",
  quota: "Limits",
  tracing: "Tracing",
  synthetic: "Synthetic",
  schedule: "Access hours",
  budget: "Budget",
  allowlist: "Models",
  guardrails: "Guardrails",
  compression: "Compression",
};

export type SettingsSaveResult<S extends string> =
  | { ok: true }
  | { ok: false; error: string; saved: S[] };

function jsonField<T>(v: FormDataEntryValue | null): { ok: true; value: T | null } | { ok: false } {
  const raw = trimmed(v);
  if (!raw) return { ok: true, value: null };
  try {
    return { ok: true, value: JSON.parse(raw) as T };
  } catch {
    return { ok: false };
  }
}

/**
 * Save a tenant's settings form. `formData` carries `id`, `sections` (the
 * changed ones, comma-separated) and the fields; JSON fields
 * (`weekly_windows`, `guardrails_policy`, `compression_policy`) are
 * blank for none.
 */
export async function saveTenantSettingsAction(formData: FormData): Promise<SettingsSaveResult<TenantSettingsSection>> {
  const session = await requireAdmin();
  const audit = { auditActor: session.email };
  const id = trimmed(formData.get("id"));
  const wanted = new Set(trimmed(formData.get("sections")).split(",").map((x) => x.trim()));
  const sections = TENANT_SECTIONS.filter((x) => wanted.has(x));
  const saved: TenantSettingsSection[] = [];
  if (!id) return { ok: false, error: "Missing tenant id.", saved };

  const refresh = () => {
    updateTag(CACHE_TAGS.tenants);
    updateTag(CACHE_TAGS.keys);
    revalidatePath("/tenants");
    revalidatePath("/fairshare");
    revalidatePath("/");
  };
  const fail = (section: TenantSettingsSection, error: string) => {
    refresh();
    return { ok: false as const, error: `${TENANT_SECTION_LABELS[section]}: ${error}`, saved };
  };

  for (const section of sections) {
    try {
      if (section === "profile") {
        const parsed = tenantUpdateSchema.safeParse({
          id,
          name: formData.get("name"),
          description: formData.get("description"),
          organization: formData.get("organization"),
          contact_email: formData.get("contact_email"),
        });
        if (!parsed.success) return fail(section, firstIssue(parsed.error));
        await obleth.updateTenant(id, {
          name: parsed.data.name,
          description: parsed.data.description,
          organization: parsed.data.organization,
          contact_email: parsed.data.contact_email ?? "",
        }, audit);
      } else if (section === "group") {
        const group = trimmed(formData.get("fairshare_group"));
        if (!group || group.length > 64) return fail(section, "Name the group (up to 64 characters).");
        // A group must exist before a tenant can join it; a new name starts one at weight 100.
        const groups = await obleth.listFairshareGroups();
        if (!groups.some((g) => g.name === group)) await obleth.createFairshareGroup(group, 100, audit);
        await obleth.setTenantGroup(id, group, audit);
      } else if (section === "weight") {
        const weight = Number(trimmed(formData.get("weight")));
        if (!Number.isInteger(weight) || weight < 1) return fail(section, "Weight must be a whole number of at least 1.");
        await obleth.setWeight(id, weight, audit);
      } else if (section === "quota") {
        const tpm = numOrUndef(formData.get("tokens_per_minute")) ?? 0;
        const mif = numOrNull(formData.get("max_in_flight"));
        if (!Number.isInteger(tpm) || tpm < 0) return fail(section, "Token rate must be a whole number, or blank for no limit.");
        if (mif !== null && (!Number.isInteger(mif) || mif < 1)) return fail(section, "In flight must be at least 1, or blank for no limit.");
        await obleth.setQuota(id, tpm, mif, audit);
      } else if (section === "tracing") {
        await obleth.setTenantTracing(id, formData.get("tracing_enabled") === "on", audit);
      } else if (section === "synthetic") {
        await obleth.setTenantSynthetic(id, formData.get("synthetic") === "on", audit);
      } else if (section === "schedule") {
        const timezone = trimmed(formData.get("timezone")) || "UTC";
        const windows = parseWeeklyWindows(formData.get("weekly_windows"));
        if (!windows.success) return fail(section, firstIssue(windows.error));
        const from = trimmed(formData.get("active_from")) || null;
        const until = trimmed(formData.get("active_until")) || null;
        if (from && until && new Date(until) <= new Date(from)) return fail(section, "The end must be after the start.");
        await obleth.setTenantSchedule(id, {
          timezone,
          active_from: from,
          active_until: until,
          weekly_windows: windows.data.length ? windows.data : null,
        }, audit);
      } else if (section === "budget") {
        const tokens = numOrNull(formData.get("budget_tokens"));
        const cost = numOrNull(formData.get("budget_cost_usd"));
        if ((tokens !== null && tokens < 0) || (cost !== null && cost < 0)) return fail(section, "Caps can't be negative.");
        const period = trimmed(formData.get("budget_period")) || "lifetime";
        await obleth.setTenantBudget(id, {
          budget_tokens: tokens,
          budget_cost_usd: cost,
          budget_period: tokens === null && cost === null ? null : period,
          ...(formData.get("budget_restart") === "on" ? { budget_started_at: new Date().toISOString() } : {}),
        }, audit);
      } else if (section === "allowlist") {
        if (!formData.has("has_allowlist")) continue;
        const models = formData.getAll("allowed_model").map((m) => trimmed(m)).filter(Boolean);
        await obleth.setTenantAllowlist(id, formData.get("allow_all") === "on" ? [] : models, audit);
      } else if (section === "guardrails") {
        const policy = jsonField<GuardrailsPolicy>(formData.get("guardrails_policy"));
        if (!policy.ok) return fail(section, "The policy could not be read.");
        await obleth.setTenantGuardrails(id, policy.value, audit);
      } else if (section === "compression") {
        const policy = jsonField<CompressionPolicy>(formData.get("compression_policy"));
        if (!policy.ok) return fail(section, "The policy could not be read.");
        await obleth.setTenantCompression(id, policy.value, audit);
      }
      saved.push(section);
    } catch (e) {
      return fail(section, actionError(e).error);
    }
  }
  refresh();
  return { ok: true };
}

export type KeySettingsSection = "key" | "tracing" | "end_user";

/** Save a key's panel: its fields (one update) and its tracing switch, each only when changed. */
export async function saveKeySettingsAction(formData: FormData): Promise<SettingsSaveResult<KeySettingsSection>> {
  const session = await requireAdmin();
  const id = trimmed(formData.get("id"));
  const wanted = new Set(trimmed(formData.get("sections")).split(","));
  const saved: KeySettingsSection[] = [];
  if (!id) return { ok: false, error: "Missing key id.", saved };
  if (wanted.has("key")) {
    const result = await updateKeyAction(formData);
    if (!result.ok) return { ok: false, error: result.error, saved };
    saved.push("key");
  }
  if (wanted.has("tracing")) {
    try {
      await obleth.setKeyTracing(id, formData.get("tracing_enabled") === "on", { auditActor: session.email });
    } catch (e) {
      return { ok: false, error: `Tracing: ${actionError(e).error}`, saved };
    }
    saved.push("tracing");
  }
  if (wanted.has("end_user")) {
    try {
      await obleth.setKeyEndUserFairshare(id, formData.get("end_user_fairshare") === "on", { auditActor: session.email });
    } catch (e) {
      return { ok: false, error: `Per-user fairshare: ${actionError(e).error}`, saved };
    }
    saved.push("end_user");
  }
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
  revalidatePath("/tenants");
  return { ok: true };
}

export interface BulkKeyResult {
  done: number;
  failed: { name: string; error: string }[];
}

/** Run `task` over the chosen keys, a few at a time, collecting failures by name. */
async function eachKey(ids: string[], task: (key: ApiKey) => Promise<void>): Promise<BulkKeyResult> {
  const wanted = new Set(ids);
  const keys = (await obleth.listKeys()).filter((k) => wanted.has(k.id));
  const failed: BulkKeyResult["failed"] = [];
  let done = 0;
  for (let i = 0; i < keys.length; i += 8) {
    await Promise.all(
      keys.slice(i, i + 8).map(async (key) => {
        try {
          await task(key);
          done += 1;
        } catch (e) {
          failed.push({ name: key.name || key.key_prefix, error: actionError(e).error });
        }
      }),
    );
  }
  updateTag(CACHE_TAGS.keys);
  revalidatePath("/keys");
  revalidatePath("/tenants");
  revalidatePath("/");
  return { done, failed };
}

export async function setKeysDisabledAction(ids: string[], disabled: boolean): Promise<BulkKeyResult> {
  const session = await requireAdmin();
  return eachKey(ids, async (key) => {
    if (key.disabled === disabled) return;
    await obleth.setKeyDisabled(key.id, disabled, { auditActor: session.email });
  });
}

/** Give every chosen key the same budget (blank caps clear it). Their other settings stay. */
export async function setKeysBudgetAction(
  ids: string[],
  budget: { budget_tokens: number | null; budget_cost_usd: number | null; budget_period: string },
): Promise<BulkKeyResult> {
  const session = await requireAdmin();
  const capped = budget.budget_tokens != null || budget.budget_cost_usd != null;
  if ((budget.budget_tokens ?? 0) < 0 || (budget.budget_cost_usd ?? 0) < 0) return { done: 0, failed: [{ name: "all", error: "Caps can't be negative." }] };
  return eachKey(ids, async (key) => {
    await obleth.updateKey(key.id, {
      name: key.name,
      description: key.description,
      weight: key.weight,
      max_in_flight: key.max_in_flight,
      budget_tokens: budget.budget_tokens,
      budget_cost_usd: budget.budget_cost_usd,
      budget_period: capped ? budget.budget_period : null,
      budget_started_at: capped ? key.budget_started_at : null,
    }, { auditActor: session.email });
  });
}

export type MoveTarget =
  | { tenantId: string }
  | { newTenant: { name: string; copyFrom?: string } };

/**
 * Move keys to another tenant, or to a new one made for them (optionally
 * with the limits, budget, hours, models and policies of `copyFrom`, and
 * placed in its fairshare group). Their secrets keep working; past usage
 * stays under the tenant it was recorded in.
 */
export async function moveKeysAction(ids: string[], target: MoveTarget): Promise<BulkKeyResult & { tenantId?: string; error?: string }> {
  const session = await requireAdmin();
  const audit = { auditActor: session.email };
  let tenantId: string;
  if ("tenantId" in target) {
    tenantId = target.tenantId;
  } else {
    const name = target.newTenant.name.trim();
    if (!name) return { done: 0, failed: [], error: "Name the new tenant." };
    try {
      const source = target.newTenant.copyFrom ? ((await obleth.listTenants()).find((t) => t.id === target.newTenant.copyFrom) ?? null) : null;
      const created = await obleth.createTenant({
        name,
        weight: source?.weight ?? 100,
        tokens_per_minute: source?.tokens_per_minute ?? 0,
        max_in_flight: source?.max_in_flight ?? null,
        fairshare_group: source?.fairshare_group || undefined,
      }, audit);
      tenantId = created.id;
      if (source) {
        await obleth.setTenantSchedule(tenantId, {
          timezone: source.timezone,
          active_from: source.active_from,
          active_until: source.active_until,
          weekly_windows: source.weekly_windows,
        }, audit);
        if (source.budget_tokens != null || source.budget_cost_usd != null) {
          await obleth.setTenantBudget(tenantId, { budget_tokens: source.budget_tokens, budget_cost_usd: source.budget_cost_usd, budget_period: source.budget_period }, audit);
        }
        if (source.allowed_models?.length) await obleth.setTenantAllowlist(tenantId, source.allowed_models, audit);
        if (source.guardrails_policy) await obleth.setTenantGuardrails(tenantId, source.guardrails_policy, audit);
        if (source.compression_policy) await obleth.setTenantCompression(tenantId, source.compression_policy, audit);
      }
    } catch (e) {
      return { done: 0, failed: [], error: actionError(e).error };
    }
    updateTag(CACHE_TAGS.tenants);
  }
  const result = await eachKey(ids, async (key) => {
    if (key.tenant_id === tenantId) return;
    await obleth.moveKey(key.id, tenantId, audit);
  });
  revalidatePath("/fairshare");
  return { ...result, tenantId };
}

/**
 * A new key with this key's tenant and settings, for a client to switch to.
 * The old key keeps working until it is turned off.
 */
export async function replaceKeyAction(id: string): Promise<{ ok: true; secret: string; key: ApiKey } | { ok: false; error: string }> {
  const session = await requireAdmin();
  try {
    const old = (await obleth.listKeys()).find((k) => k.id === id);
    if (!old) return { ok: false, error: "Key not found." };
    if (old.kind === "identity") return { ok: false, error: "An identity key has no secret to replace." };
    const created = await obleth.createKey(old.tenant_id, {
      name: `${old.name} (new)`,
      description: old.description,
      weight: old.weight,
      max_in_flight: old.max_in_flight,
      budget_tokens: old.budget_tokens,
      budget_cost_usd: old.budget_cost_usd,
      budget_period: old.budget_period,
      budget_started_at: old.budget_started_at,
    }, { auditActor: session.email });
    if (old.tracing_enabled) await obleth.setKeyTracing(created.key.id, true, { auditActor: session.email });
    if (old.end_user_fairshare) await obleth.setKeyEndUserFairshare(created.key.id, true, { auditActor: session.email });
    updateTag(CACHE_TAGS.keys);
    revalidatePath("/keys");
    return { ok: true, secret: created.secret, key: created.key };
  } catch (e) {
    return actionError(e);
  }
}

export async function deleteKeyAction(id: string): Promise<ActionResult> {
  const session = await requireAdmin();
  return deleteAndRevalidate(
    () => obleth.deleteKey(id, { auditActor: session.email }),
    () => {
      updateTag(CACHE_TAGS.keys);
      revalidatePath("/keys");
      revalidatePath("/");
    },
  );
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

export async function setCapacityAction(max: number) {
  const session = await requireAdmin();
  await obleth.setCapacity(max, { auditActor: session.email });
  revalidatePath("/");
  revalidatePath("/fairshare");
}

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
  updateTag(CACHE_TAGS.models);
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

export async function deleteModelAction(id: string): Promise<ActionResult> {
  const session = await requireAdmin();
  return deleteAndRevalidate(
    () => obleth.deleteModel(id, { auditActor: session.email }),
    () => {
      updateTag(CACHE_TAGS.models);
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

// Full replacement body for PUT /models/{id}, built from the current model so a
// partial edit re-sends every field the gateway expects. `api_key` is omitted
// on purpose: it is a write-only secret not returned by listModels, and sending
// null would clear it. `variants` is left out too: omitted, the gateway keeps
// them. Callers spread this and override only their own fields.
function toModelUpdateBody(model: ModelRoute) {
  return {
    upstream_model: model.upstream_model,
    api_base: model.api_base,
    model_type: model.model_type,
    quantization: model.quantization,
    aliases: model.aliases ?? [],
    description: model.description,
    input_cost_per_token: model.input_cost_per_token,
    output_cost_per_token: model.output_cost_per_token,
    cost_per_image: model.cost_per_image,
    cost_per_audio_second: model.cost_per_audio_second,
    cost_per_character: model.cost_per_character,
    cost_per_video: model.cost_per_video,
    energy_slots_per_node: model.energy_slots_per_node,
    route_bias: model.route_bias,
    auto_eligible: model.auto_eligible,
    draft_model: model.draft_model,
    verify_api_base: model.verify_api_base,
    verify_upstream_model: model.verify_upstream_model,
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

// ----------------------------------------------------------------------------
// The model page's settings form
//
// Every setting on a model's page submits as one form and saves with one
// button. The client names the sections that changed; each section maps to
// the admin endpoint that owns it, so an untouched section is never written.
// ----------------------------------------------------------------------------

/** The parts of a model's settings form, each saved through its own endpoint. */
export type ModelSettingsSection = "model" | "cache" | "reliability" | "health" | "capacity" | "knowledge";

const MODEL_SETTINGS_SECTIONS: readonly ModelSettingsSection[] = ["model", "cache", "reliability", "health", "capacity", "knowledge"];

const SECTION_LABELS: Record<ModelSettingsSection, string> = {
  model: "Model settings",
  cache: "Response cache",
  reliability: "Delivery",
  health: "Health checks",
  capacity: "Capacity",
  knowledge: "Knowledge collections",
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
    ...toModelUpdateBody(current),
    description: text("description", current.description),
    model_type: text("model_type", current.model_type),
    quantization: text("quantization", current.quantization),
    ...(has("aliases") ? { aliases: aliasesFromForm(formData) } : {}),
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
    updateTag(CACHE_TAGS.models);
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
    await obleth.updateModel(id, { ...toModelUpdateBody(current), enabled }, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.models);
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
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/");
  return { ok: true };
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
    updateTag(CACHE_TAGS.models);
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
  updateTag(CACHE_TAGS.models);
  revalidatePath("/models");
  revalidatePath("/");
  return { done, failed };
}

export async function setModelsEnabledAction(ids: string[], enabled: boolean): Promise<BulkModelResult> {
  const session = await requireAdmin();
  return eachModel(ids, async (model) => {
    if (model.enabled === enabled) return;
    await obleth.updateModel(model.id, { ...toModelUpdateBody(model), enabled }, { auditActor: session.email });
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
  updateTag(CACHE_TAGS.models);
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

/** Turn an MCP server on or off. Off, its tools disappear from every model and `/mcp/<name>` answers 403. */
export async function setMcpServerEnabledAction(id: string, enabled: boolean): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    const server = (await obleth.listMcpServers()).find((m) => m.id === id);
    if (!server) return { ok: false, error: "That server no longer exists." };
    await obleth.updateMcpServer(id, { upstream_url: server.upstream_url, enabled }, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/mcp");
  return { ok: true };
}

/**
 * Save an MCP server's name, URL and key. A blank key keeps the stored one;
 * `clearAuth` removes it. Renaming carries the new name into every model grant.
 */
export async function saveMcpServerAction(input: { id: string; name: string; upstreamUrl: string; authHeader?: string; clearAuth?: boolean }): Promise<ActionResult & { name?: string }> {
  const session = await requireAdmin();
  const name = input.name.trim();
  if (!MCP_NAME_RE.test(name)) return { ok: false, error: MCP_NAME_HINT };
  const url = input.upstreamUrl.trim();
  if (!z.string().url().safeParse(url).success) return { ok: false, error: "A valid upstream URL is required." };
  try {
    const saved = await obleth.updateMcpServer(input.id, {
      upstream_url: url,
      name,
      ...(input.authHeader?.trim() ? { auth_header: input.authHeader.trim() } : {}),
      clear_auth: !!input.clearAuth && !input.authHeader?.trim(),
    }, { auditActor: session.email });
    revalidatePath("/mcp");
    revalidatePath("/models");
    return { ok: true, name: saved.name };
  } catch (e) {
    return actionError(e);
  }
}

/** Grant a model an MCP server's tools, or take them away. */
export async function setModelToolServerAction(modelId: string, server: string, on: boolean): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    const model = await loadModel(modelId);
    if (!model) return { ok: false, error: "That model no longer exists." };
    const current = model.tool_servers ?? [];
    const next = on ? [...new Set([...current, server])] : current.filter((s) => s !== server);
    if (next.length === current.length && next.every((s, i) => s === current[i])) return { ok: true };
    await obleth.updateModel(modelId, { ...toModelUpdateBody(model), tool_servers: next }, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/mcp");
  revalidatePath("/models");
  return { ok: true };
}

export async function deleteMcpServerAction(id: string): Promise<ActionResult> {
  const session = await requireAdmin();
  return deleteAndRevalidate(
    () => obleth.deleteMcpServer(id, { auditActor: session.email }),
    () => revalidatePath("/mcp"),
  );
}

// ----------------------------------------------------------------------------
// The Settings page
//
// Every section submits as one form (`section.field` names) and saves with one
// button. Each changed section goes through the action that owns it, in a
// fixed order; a refusal says which sections were already saved.
// ----------------------------------------------------------------------------

type SettingsPageSection = "alerts" | "routing" | "boons" | "energy" | "assistant" | "retention";
const SETTINGS_PAGE_ORDER: SettingsPageSection[] = ["alerts", "routing", "boons", "energy", "assistant", "retention"];
const SETTINGS_PAGE_LABEL: Record<SettingsPageSection, string> = { alerts: "Alerts", routing: "Routing", boons: "Boons", energy: "Energy", assistant: "Assistant", retention: "Data" };

const BOON_BOOLS = [
  "vision_enabled", "structured_output_enabled", "tool_loop_enabled", "image_generation_enabled", "web_search_enabled", "speculation_enabled",
  "compression_enabled", "compression_code_compaction", "compression_dedup", "compression_compact_logs", "compression_allow_lossy",
] as const;
const BOON_NUMBERS = [
  "vision_max_images", "vision_timeout_ms", "structured_output_max_repair_attempts", "structured_output_timeout_ms",
  "tool_loop_max_turns", "tool_loop_tool_timeout_ms", "tool_loop_deadline_secs",
  "image_generation_max_images_per_request", "image_generation_timeout_ms",
  "web_search_max_results", "web_search_max_searches_per_request", "web_search_timeout_ms",
  "speculation_agree_min", "speculation_lp_min", "speculation_abort_agree", "speculation_abort_lp", "speculation_first_chunk_tokens",
  "speculation_chunk_tokens", "speculation_decide_by_tokens", "speculation_max_draft_tokens", "speculation_pace_ms", "speculation_timeout_ms",
  "compression_min_tokens", "compression_max_segments", "compression_max_lossy_segments", "compression_original_ttl_secs", "compression_neural_keep_ratio",
] as const;
/** Model pickers: blank means none. */
const BOON_MODELS = ["vision_fallback_model", "structured_output_fixer_model", "image_generation_model", "web_search_tool", "speculation_draft_model", "speculation_classify_model"] as const;
const BOON_TEXTS = ["vision_describe_prompt", "tool_loop_nudge", "image_generation_tool_description", "web_search_tool_description", "speculation_verify_url_template"] as const;

/** The boons section of the Settings form as the gateway's update, or why not. */
function boonSettingsFromForm(formData: FormData): { ok: true; body: UpdateBoonSettings } | { ok: false; error: string } {
  const get = (k: string) => formData.get(`boons.${k}`);
  const body: Record<string, unknown> = {};
  for (const k of BOON_BOOLS) body[k] = get(k) === "on";
  for (const k of BOON_NUMBERS) {
    const raw = trimmed(get(k));
    if (raw === "") continue;
    const n = Number(raw);
    if (!Number.isFinite(n)) return { ok: false, error: `${k.replace(/_/g, " ")} must be a number.` };
    body[k] = n;
  }
  for (const k of BOON_MODELS) body[k] = trimmed(get(k)) || null;
  for (const k of BOON_TEXTS) if (formData.has(`boons.${k}`)) body[k] = String(get(k) ?? "");
  body.image_generation_allowed_sizes = trimmed(get("image_generation_allowed_sizes")).split(/[\s,]+/).filter(Boolean);
  body.speculation_unlisted_categories_speculate = get("speculation_unlisted") === "on";
  const gates = trimmed(get("speculation_gates"));
  try {
    body.speculation_category_gates = gates ? (JSON.parse(gates) as SpeculationCategoryGate[]) : [];
  } catch {
    return { ok: false, error: "The category rules couldn't be read." };
  }
  const kwargs = trimmed(get("speculation_draft_chat_template_kwargs"));
  if (kwargs) {
    try {
      const parsed = JSON.parse(kwargs);
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return { ok: false, error: "The drafter's template arguments must be a JSON object." };
      body.speculation_draft_chat_template_kwargs = parsed;
    } catch {
      return { ok: false, error: "The drafter's template arguments aren't valid JSON." };
    }
  } else {
    body.speculation_draft_chat_template_kwargs = {};
  }
  const attempts = body.structured_output_max_repair_attempts as number | undefined;
  if (attempts !== undefined && (attempts < 0 || attempts > 3)) return { ok: false, error: "Structured output: 0 to 3 retries." };
  const turns = body.tool_loop_max_turns as number | undefined;
  if (turns !== undefined && (turns < 1 || turns > 8)) return { ok: false, error: "Tool loop: 1 to 8 turns." };
  const images = body.image_generation_max_images_per_request as number | undefined;
  if (images !== undefined && (images < 1 || images > 4)) return { ok: false, error: "Image generation: 1 to 4 images a call." };
  const results = body.web_search_max_results as number | undefined;
  if (results !== undefined && (results < 1 || results > 10)) return { ok: false, error: "Web search: 1 to 10 results a search." };
  const searches = body.web_search_max_searches_per_request as number | undefined;
  if (searches !== undefined && (searches < 1 || searches > 8)) return { ok: false, error: "Web search: 1 to 8 searches a request." };
  const keep = body.compression_neural_keep_ratio as number | undefined;
  if (keep !== undefined && (keep < 0.05 || keep > 1)) return { ok: false, error: "Compression: keep between 5% and 100% of prose." };
  return { ok: true, body: body as UpdateBoonSettings };
}

export async function saveSettingsAction(formData: FormData): Promise<SettingsSaveResult<SettingsPageSection>> {
  await requireAdmin();
  const wanted = new Set(trimmed(formData.get("sections")).split(",").map((x) => x.trim()));
  const saved: SettingsPageSection[] = [];
  const g = (section: string, k: string) => formData.get(`${section}.${k}`);
  const num = (section: string, k: string) => {
    const raw = trimmed(g(section, k));
    return raw === "" ? undefined : Number(raw);
  };
  const fail = (section: SettingsPageSection, error: string) => ({ ok: false as const, error: `${SETTINGS_PAGE_LABEL[section]}: ${error}`, saved });

  for (const section of SETTINGS_PAGE_ORDER.filter((x) => wanted.has(x))) {
    let res: ActionResult;
    if (section === "alerts") {
      const minutes = num("alerts", "quiet_minutes");
      if (minutes !== undefined && (!Number.isFinite(minutes) || minutes < 0)) return fail(section, "The quiet period is minutes, 0 or more.");
      const body: UpdateAlertSettings = { min_interval_secs: Math.round((minutes ?? 5) * 60) };
      const webhook = trimmed(g("alerts", "slack_webhook_url"));
      if (webhook) body.slack_webhook_url = webhook;
      else if (g("alerts", "clear_slack") === "on") body.clear_slack_webhook = true;
      if (g("alerts", "email_enabled") === "on") {
        const host = trimmed(g("alerts", "smtp_host"));
        const from = trimmed(g("alerts", "from_address"));
        const recipients = trimmed(g("alerts", "recipients")).split(/[\n,]/).map((r) => r.trim()).filter(Boolean);
        if (!host || !from || recipients.length === 0) return fail(section, "Email needs a server, a from address and at least one recipient.");
        body.email = {
          smtp_host: host,
          smtp_port: num("alerts", "smtp_port") || 587,
          username: trimmed(g("alerts", "smtp_username")) || null,
          from_address: from,
          recipients,
          starttls: g("alerts", "starttls") === "on",
        };
        const password = trimmed(g("alerts", "smtp_password"));
        if (password) body.email.smtp_password = password;
        else if (g("alerts", "clear_password") === "on") body.email.clear_smtp_password = true;
      } else {
        body.email = null;
      }
      res = await setAlertSettingsAction(body);
    } else if (section === "routing") {
      res = await setAutoRouterSettingsAction({
        classifier_enabled: g("routing", "classifier_enabled") === "on",
        classifier_model: trimmed(g("routing", "classifier_model")),
        classifier_timeout_ms: num("routing", "classifier_timeout_ms"),
        capacity_weight: num("routing", "capacity_weight"),
        cost_weight: num("routing", "cost_weight"),
        tag_weight: num("routing", "tag_weight"),
        temperature: num("routing", "temperature"),
        default_soft_cap: num("routing", "soft_cap"),
        difficulty_enabled: g("routing", "difficulty") === "on",
        tier_source: (trimmed(g("routing", "tier_source")) || "hybrid") as "hybrid" | "derived" | "declared",
        messages_default_model: trimmed(g("routing", "messages_default_model")),
      });
    } else if (section === "boons") {
      const parsed = boonSettingsFromForm(formData);
      if (!parsed.ok) return fail(section, parsed.error);
      res = await setBoonSettingsAction(parsed.body);
    } else if (section === "energy") {
      const body: UpdateEnergySettings = {
        enabled: g("energy", "enabled") === "on",
        prometheus_url: trimmed(g("energy", "prometheus_url")),
        power_query: String(g("energy", "power_query") ?? "").trim(),
        poll_interval_secs: num("energy", "poll_interval_secs"),
        energy_cost_per_kwh: num("energy", "energy_cost_per_kwh"),
        carbon_g_per_kwh: num("energy", "carbon_g_per_kwh"),
        pue: num("energy", "pue"),
      };
      if (body.poll_interval_secs !== undefined && body.poll_interval_secs < 10) return fail(section, "Poll at most every 10 seconds.");
      if (body.pue !== undefined && body.pue < 1) return fail(section, "PUE is 1 or more.");
      if (body.enabled && !body.prometheus_url) return fail(section, "Energy needs a Prometheus URL.");
      res = await setEnergySettingsAction(body);
    } else if (section === "assistant") {
      const limits = ["bench_max_concurrency", "bench_max_duration_s", "bench_max_requests"].map((k) => num("assistant", k));
      if (limits.some((v) => v === undefined || !Number.isInteger(v) || v < 1)) return fail(section, "Benchmark limits are whole numbers of 1 or more.");
      res = await setCharoSettingsAction({
        enabled: g("assistant", "enabled") === "on",
        brain_model: trimmed(g("assistant", "brain_model")) || null,
        tools_enabled: { run_benchmark: g("assistant", "tool_run_benchmark") === "on" },
        bench_max_concurrency: limits[0]!,
        bench_max_duration_s: limits[1]!,
        bench_max_requests: limits[2]!,
      });
    } else {
      res = await setUsageRetentionAction(num("retention", "days") ?? 0);
    }
    if (!res.ok) return fail(section, res.error);
    saved.push(section);
  }
  revalidatePath("/settings");
  return { ok: true };
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
  messages_default_model: z.string().optional(),
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

/** Turn knowledge retrieval on or off for every model that has the boon and a collection. */
export async function setKnowledgeEnabledAction(enabled: boolean): Promise<ActionResult> {
  return setKnowledgeSettingsAction({ enabled });
}

/** The Knowledge page's retrieval settings form, saved as one. */
export async function saveRetrievalSettingsAction(formData: FormData): Promise<SettingsSaveResult<"retrieval">> {
  const num = (k: string) => {
    const v = trimmed(formData.get(k));
    return v === "" ? undefined : Number(v);
  };
  const body: UpdateKnowledgeSettings = {
    enabled: formData.get("enabled") === "on",
    top_k: num("top_k"),
    min_score: num("min_score"),
    max_context_tokens: num("max_context_tokens"),
    query_turns: num("query_turns"),
    embed_timeout_ms: num("embed_timeout_ms"),
    query_cache_ttl_s: num("query_cache_ttl_s"),
    max_upload_bytes: num("max_upload_mb") === undefined ? undefined : Math.round(num("max_upload_mb")! * 1024 * 1024),
    max_chunks_per_collection: num("max_chunks_per_collection"),
    index_batch_size: num("index_batch_size"),
    index_timeout_ms: num("index_timeout_ms"),
    index_stale_after_secs: num("index_stale_after_secs"),
    debug_snapshot: formData.get("debug_snapshot") === "on",
  };
  const bad = Object.entries(body).find(([, v]) => typeof v === "number" && (!Number.isFinite(v) || v < 0));
  if (bad) return { ok: false, error: `${bad[0].replace(/_/g, " ")} must be a number of 0 or more.`, saved: [] };
  if (body.min_score !== undefined && body.min_score > 1) return { ok: false, error: "Minimum score is between 0 and 1.", saved: [] };
  const res = await setKnowledgeSettingsAction(body);
  if (!res.ok) return { ok: false, error: res.error, saved: [] };
  revalidatePath("/knowledge");
  return { ok: true };
}

/**
 * Attach a collection to a model, or detach it. Attaching also grants the
 * model the Knowledge boon, which retrieval needs; detaching the last
 * collection leaves the boon for the model page to manage.
 */
export async function attachCollectionAction(modelId: string, collectionId: string, attach: boolean): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    const model = await loadModel(modelId);
    if (!model) return { ok: false, error: "That model no longer exists." };
    const { collection_ids } = await obleth.getModelCollections(modelId);
    const next = attach ? [...new Set([...collection_ids, collectionId])] : collection_ids.filter((c) => c !== collectionId);
    await obleth.setModelCollections(modelId, next, { auditActor: session.email });
    if (attach && !(model.boons ?? []).includes("knowledge")) {
      await obleth.updateModel(modelId, { ...toModelUpdateBody(model), boons: [...(model.boons ?? []), "knowledge"] }, { auditActor: session.email });
    }
  } catch (e) {
    return actionError(e);
  }
  updateTag(CACHE_TAGS.models);
  revalidatePath("/knowledge");
  revalidatePath("/models");
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
  revalidatePath("/knowledge");
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
  revalidatePath("/deployments");
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

/**
 * Rebuild the data plane's resolver cache (keys, models, MCP servers) from
 * Postgres. This is the retry a failed delete-eviction error points operators at.
 */
export async function resyncCacheAction(): Promise<ActionResult & { report?: ResyncReport }> {
  const session = await requireAdmin();
  try {
    const report = await obleth.resync({ auditActor: session.email });
    return { ok: true, report };
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
    // 0 = "Auto": saved bare, the level derives from cost rank under hybrid
    // tier sourcing. An explicit level — INCLUDING 1 — keeps its suffix, so
    // "pinned weak on purpose" survives the round trip.
    tags.push(level > 0 ? `${base}:${level}` : base);
  }
  return tags;
}

// Parses the aliases textarea (one name per line, commas also accepted) into
// the list the gateway stores. Order is preserved; the gateway does the
// trimming, de-duplication, and collision checks, so a name rejected there
// surfaces as a save error rather than being silently dropped here.
function aliasesFromForm(formData: FormData): string[] {
  return String(formData.get("aliases") ?? "")
    .split(/[\n,]/)
    .map((a) => a.trim())
    .filter((a) => a.length > 0);
}

// Parses the variants field (the settings page's rows, as JSON) into the list
// the gateway stores. Rows without a name are dropped, as blank aliases are;
// the gateway checks the rest (names already taken, unknown boons, the cap),
// so a variant it refuses surfaces as a save error.
function variantsFromForm(formData: FormData): ModelVariantInput[] {
  let raw: unknown;
  try {
    raw = JSON.parse(String(formData.get("variants") || "[]"));
  } catch {
    raw = null;
  }
  if (!Array.isArray(raw)) throw new Error("The variants could not be read. Reload the page and try again.");
  return raw.flatMap((item: unknown) => {
    const v = (item && typeof item === "object" ? item : {}) as Record<string, unknown>;
    const name = typeof v.name === "string" ? v.name.trim() : "";
    if (!name) return [];
    return [{
      name,
      description: typeof v.description === "string" ? v.description.trim() : "",
      boons: Array.isArray(v.boons) ? v.boons.filter((b): b is string => typeof b === "string") : [],
    }];
  });
}

// The upstream-headers textarea as an update field, only when the form has
// one: a form without it (every other model tab) must leave the stored
// headers alone, which the gateway does when the field is omitted.
function upstreamHeadersFromForm(
  formData: FormData,
): { upstream_headers?: Record<string, string | null> } {
  if (!formData.has("upstream_headers")) return {};
  return { upstream_headers: parseUpstreamHeaders(String(formData.get("upstream_headers") ?? "")) };
}

function clampTagLevel(raw: FormDataEntryValue | null): number {
  const n = Number(raw);
  if (!Number.isFinite(n)) return 0;
  return Math.min(3, Math.max(0, Math.trunc(n)));
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

// ----------------------------------------------------------------------------
// Deployments
//
// A Slurm deployment is a model plus its managed spec. The spec is written
// whole, so every change here reads the stored spec, changes what was asked,
// and puts it back.
// ----------------------------------------------------------------------------

function refreshDeployments() {
  updateTag(CACHE_TAGS.models);
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
  const errors = validateManagedModelForm(values);
  const first = Object.values(errors)[0];
  if (first) return { ok: false, error: first, saved: [] };
  const text = (k: string) => (values[`slurm_${k}`] ?? "").trim();
  const nullable = (k: string) => text(k) || null;
  const int = (k: string) => Number(text(k));
  const res = await changeSpec(id, (b) => {
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
    const models = await obleth.listModelsFresh();
    if (models.some((m) => m.model_name === createBody.model_name || m.aliases?.includes(createBody.model_name) || m.variants?.some((v) => v.name === createBody.model_name))) {
      return { ok: false, error: `A model called ${createBody.model_name} already exists. Pick another name.` };
    }
    if (!managedBody.partition) return { ok: false, error: "Pick a partition." };
    const created = await obleth.createModel({
      model_name: createBody.model_name,
      upstream_model: createBody.upstream_model,
      api_base: createBody.api_base,
      model_type: createBody.model_type,
    }, { auditActor: session.email });
    await obleth.putManagedModel(created.id, managedBody, { auditActor: session.email });
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
    const ls = (spec.launcher_spec ?? {}) as { recipe_id?: string; inputs?: Record<string, string>; env?: Record<string, string> };
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
    return await saveRecipeFromFormAction(ls.recipe_id, form, { name, model: model_ });
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
