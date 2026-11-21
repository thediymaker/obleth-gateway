"use server";

/** API keys: create and update, the key panel's settings, and bulk key changes. */

import { revalidatePath } from "next/cache";
import { obleth, OblethApiError, type KeyListQuery } from "@/lib/obleth";
import type { ApiKey } from "@/lib/obleth";
import { requireAdmin } from "@/lib/auth/roles";
import { countryCodes, networkLines } from "@/lib/key-networks";
import { actionError, type ActionResult } from "@/lib/action-result";
import { formatNumber } from "@/lib/format";
import { trimmed } from "@/lib/form-helpers";
import { deleteAndRevalidate, firstIssue, keyCreateSchema, keyUpdateSchema, SettingsSaveResult } from "./shared";

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
        allowed_networks: networkLines(formData.get("allowed_networks")),
        allowed_countries: countryCodes(formData.get("allowed_countries")),
      },
      { auditActor: session.email },
    );
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
        // Only a form that shows the field sends it; left out, the key keeps its list.
        ...(formData.has("allowed_networks") ? { allowed_networks: networkLines(formData.get("allowed_networks")) } : {}),
        ...(formData.has("allowed_countries") ? { allowed_countries: countryCodes(formData.get("allowed_countries")) } : {}),
      },
      { auditActor: session.email },
    );
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/keys");
  revalidatePath("/");
  return { ok: true };
}

export type KeySettingsSection = "key" | "tracing" | "end_user" | "background";

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
  if (wanted.has("background")) {
    try {
      await obleth.setKeyBackground(id, formData.get("background") === "on", { auditActor: session.email });
    } catch (e) {
      return { ok: false, error: `Background priority: ${actionError(e).error}`, saved };
    }
    saved.push("background");
  }
  revalidatePath("/keys");
  revalidatePath("/tenants");
  return { ok: true };
}

export interface BulkKeyResult {
  done: number;
  failed: { name: string; error: string }[];
}

/** Most ids one `keysByIds` read takes. */
const KEY_IDS_PER_READ = 200;

/**
 * The keys a bulk action acts on: the rows picked on the page (ids), or every
 * key the Keys list's search and filters match, with the count the person
 * confirmed.
 */
export type KeyTarget = string[] | { filter: KeyListQuery; expected: number };

/**
 * The ids of a target's keys. A filter is matched by the gateway now; if it
 * matches a different number of keys than the person was shown, nothing is
 * done, so no key they were not told about is touched.
 */
async function targetIds(target: KeyTarget): Promise<string[]> {
  if (Array.isArray(target)) return target;
  const { sort: _sort, dir: _dir, ...filter } = target.filter;
  const ids: string[] = [];
  let cursor: string | undefined;
  do {
    const page = await obleth.listKeysPage(filter, { limit: 1000, cursor, fields: ["id"] as const });
    ids.push(...page.data.map((k) => k.id));
    cursor = page.next ?? undefined;
  } while (cursor);
  if (ids.length !== target.expected) {
    throw new Error(`${formatNumber(ids.length)} keys match now, not the ${formatNumber(target.expected)} you were shown. Nothing was changed; look again and retry.`);
  }
  return ids;
}

/**
 * Run `task` over the chosen keys, a few at a time, collecting failures by
 * name. The pages refresh whatever happened, even when a run fails partway.
 */
async function eachKey(target: KeyTarget, task: (key: ApiKey) => Promise<void>): Promise<BulkKeyResult> {
  const failed: BulkKeyResult["failed"] = [];
  let done = 0;
  try {
    const ids = await targetIds(target);
    const keys: ApiKey[] = [];
    for (let i = 0; i < ids.length; i += KEY_IDS_PER_READ) {
      keys.push(...(await obleth.keysByIds(ids.slice(i, i + KEY_IDS_PER_READ))));
    }
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
  } catch (e) {
    failed.push({ name: "all", error: actionError(e).error });
  } finally {
    revalidatePath("/keys");
    revalidatePath("/tenants");
    revalidatePath("/");
  }
  return { done, failed };
}

export async function setKeysDisabledAction(target: KeyTarget, disabled: boolean): Promise<BulkKeyResult> {
  const session = await requireAdmin();
  return eachKey(target, async (key) => {
    if (key.disabled === disabled) return;
    await obleth.setKeyDisabled(key.id, disabled, { auditActor: session.email });
  });
}

/** Give every chosen key the same budget (blank caps clear it). Their other settings stay. */
export async function setKeysBudgetAction(
  target: KeyTarget,
  budget: { budget_tokens: number | null; budget_cost_usd: number | null; budget_period: string },
): Promise<BulkKeyResult> {
  const session = await requireAdmin();
  const capped = budget.budget_tokens != null || budget.budget_cost_usd != null;
  if ((budget.budget_tokens ?? 0) < 0 || (budget.budget_cost_usd ?? 0) < 0) return { done: 0, failed: [{ name: "all", error: "Caps can't be negative." }] };
  return eachKey(target, async (key) => {
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
export async function moveKeysAction(keys: KeyTarget, target: MoveTarget): Promise<BulkKeyResult & { tenantId?: string; error?: string }> {
  const session = await requireAdmin();
  const audit = { auditActor: session.email };
  // Matched before a new tenant is made, so a changed match makes nothing.
  let ids: string[];
  try {
    ids = await targetIds(keys);
  } catch (e) {
    return { done: 0, failed: [], error: actionError(e).error };
  }
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
    const old = await obleth.getKey(id).catch((e) => {
      if (e instanceof OblethApiError && e.status === 404) return null;
      throw e;
    });
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
      allowed_networks: old.allowed_networks ?? [],
      allowed_countries: old.allowed_countries ?? [],
    }, { auditActor: session.email });
    if (old.tracing_enabled) await obleth.setKeyTracing(created.key.id, true, { auditActor: session.email });
    if (old.end_user_fairshare) await obleth.setKeyEndUserFairshare(created.key.id, true, { auditActor: session.email });
    if (old.background) await obleth.setKeyBackground(created.key.id, true, { auditActor: session.email });
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
      revalidatePath("/keys");
      revalidatePath("/");
    },
  );
}

export async function deleteKeysAction(target: KeyTarget): Promise<BulkKeyResult> {
  const session = await requireAdmin();
  return eachKey(target, (key) => obleth.deleteKey(key.id, { auditActor: session.email }));
}
