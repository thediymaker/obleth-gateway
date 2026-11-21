"use server";

/** Tenants: create, status, weight and caps, and the tenant page's settings form. */

import { revalidatePath } from "next/cache";
import { obleth } from "@/lib/obleth";
import type { CompressionPolicy, GuardrailsPolicy } from "@/lib/obleth";
import { requireAdmin } from "@/lib/auth/roles";
import { actionError, type ActionResult } from "@/lib/action-result";
import { finiteNumber, numOrNull, trimmed } from "@/lib/form-helpers";
import { deleteAndRevalidate, firstIssue, jsonField, parseWeeklyWindows, SettingsSaveResult, tenantCreateSchema, tenantUpdateSchema } from "./shared";

/**
 * Add a tenant from the Add tenant sheet: name, organization, group, weight
 * and caps. Its hours, budget, models and policies are set on its page.
 */
export async function createTenantAction(formData: FormData): Promise<{ ok: true; tenant: { id: string; name: string } } | { ok: false; error: string }> {
  const session = await requireAdmin();
  const parsed = tenantCreateSchema.safeParse({
    name: formData.get("name"),
    organization: formData.get("organization"),
    fairshare_group: formData.get("fairshare_group"),
    weight: formData.get("weight"),
    tokens_per_minute: formData.get("tokens_per_minute"),
    max_in_flight: formData.get("max_in_flight"),
  });
  if (!parsed.success) return { ok: false, error: firstIssue(parsed.error) };
  const data = parsed.data;
  try {
    const tenant = await obleth.createTenant({
      name: data.name,
      weight: data.weight ?? 100,
      tokens_per_minute: data.tokens_per_minute ?? 0,
      max_in_flight: data.max_in_flight,
      fairshare_group: data.fairshare_group || undefined,
    }, { auditActor: session.email });
    if (data.organization) {
      await obleth.updateTenant(tenant.id, { name: data.name, organization: data.organization }, { auditActor: session.email });
    }
    revalidatePath("/tenants");
    revalidatePath("/fairshare");
    revalidatePath("/");
    return { ok: true, tenant: { id: tenant.id, name: tenant.name } };
  } catch (e) {
    return actionError(e);
  }
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
      revalidatePath("/tenants");
      revalidatePath("/keys");
      revalidatePath("/fairshare");
      revalidatePath("/");
    },
  );
}

export async function setWeightAction(id: string, weight: number): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.setWeight(id, weight, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/tenants");
  revalidatePath("/fairshare");
  revalidatePath("/");
  return { ok: true };
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
    const tenant = await obleth.getTenant(id);
    await obleth.setQuota(id, tenant.tokens_per_minute, maxInFlight, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/fairshare");
  revalidatePath("/tenants");
  return { ok: true };
}

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
        const tpm = finiteNumber(formData.get("tokens_per_minute")) ?? 0;
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
