/**
 * Helpers the server actions share: input schemas, form readers and the
 * delete-then-revalidate wrapper. Not a server-action module, so it may export
 * constants and plain functions; only the actions modules import it.
 */

import { z } from "zod";
import { obleth, OblethApiError } from "@/lib/obleth";
import type { ModelRoute, ModelSessionAffinity, ModelVariantInput } from "@/lib/obleth";
import { parseUpstreamHeaders } from "@/lib/upstream-headers";
import { actionError, type ActionResult } from "@/lib/action-result";
import { blankToUndef, checkbox, finiteNumber, nonNegNumber, optionalNonNegInt, optionalNonNegNumber, optionalPositiveInt, optionalText, positiveIntWithDefault, requiredText, trimmed } from "@/lib/form-helpers";

/**
 * Run a delete and revalidate whether or not it succeeded: the Management API
 * answers 502 when the row is gone from Postgres but the data-plane cache
 * eviction failed, so the list must refresh and the operator must see that
 * message (it names the reconcile endpoint).
 */
export async function deleteAndRevalidate(
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

export function normalizeModelApiName(value: unknown) {
  return trimmed(value)
    .toLowerCase()
    .replace(/[\s_]+/g, "-")
    .replace(/[^a-z0-9.-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^[.-]+|[.-]+$/g, "");
}

export const tenantCreateSchema = z.object({
  name: requiredText("Tenant name is required"),
  organization: optionalText,
  fairshare_group: optionalText,
  weight: optionalPositiveInt,
  tokens_per_minute: optionalNonNegInt,
  max_in_flight: optionalPositiveInt,
});

export const tenantUpdateSchema = z.object({
  id: requiredText("Missing tenant id"),
  name: requiredText("Tenant name is required"),
  description: optionalText,
  organization: optionalText,
  contact_email: z.preprocess(
    blankToUndef,
    z.string().email("Invalid contact email").optional(),
  ),
});

export const modelFieldsSchema = {
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

export const modelApiName = z.preprocess(
  normalizeModelApiName,
  z
    .string()
    .min(1, "API model name is required")
    .regex(
      /^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/,
      "API model name can use lowercase letters, numbers, dashes, and dots.",
    ),
);

export const modelCreateSchema = z.object({
  model_name: modelApiName,
  ...modelFieldsSchema,
});

// Mirrors the gateway: the name is the `/mcp/<name>` path segment and the key models grant it by.
export const MCP_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

export const MCP_NAME_HINT = "Use letters, digits, '.', '_' or '-', starting with a letter or digit (up to 64).";

export const mcpCreateSchema = z.object({
  name: z.preprocess(trimmed, z.string().min(1, "Name is required").regex(MCP_NAME_RE, MCP_NAME_HINT)),
  upstream_url: z.preprocess(
    trimmed,
    z.string().url("A valid upstream URL is required"),
  ),
});

export const keyFieldsSchema = {
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

export const keyCreateSchema = z.object({
  tenant_id: requiredText("Tenant is required"),
  ...keyFieldsSchema,
});

export const keyUpdateSchema = z.object({
  id: requiredText("Missing key id"),
  ...keyFieldsSchema,
});

/** Return the first zod issue message for surfacing to the UI. */
export function firstIssue(error: z.ZodError): string {
  return error.issues[0]?.message ?? "Invalid input";
}

export const weeklyWindowSchema = z
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

export function parseWeeklyWindows(value: FormDataEntryValue | null) {
  const raw = trimmed(value);
  if (!raw) return weeklyWindowSchema.safeParse([]);
  try {
    return weeklyWindowSchema.safeParse(JSON.parse(raw));
  } catch {
    return weeklyWindowSchema.safeParse("__invalid_json__");
  }
}

export type SettingsSaveResult<S extends string> =
  | { ok: true }
  | { ok: false; error: string; saved: S[] };

export function jsonField<T>(v: FormDataEntryValue | null): { ok: true; value: T | null } | { ok: false } {
  const raw = trimmed(v);
  if (!raw) return { ok: true, value: null };
  try {
    return { ok: true, value: JSON.parse(raw) as T };
  } catch {
    return { ok: false };
  }
}

// Full replacement body for PUT /models/{id}, built from the current model so a
// partial edit re-sends every field the gateway expects. `api_key` is omitted
// on purpose: it is a write-only secret not returned by listModels, and sending
// null would clear it. `variants` is left out too: omitted, the gateway keeps
// them. Callers spread this and override only their own fields.
/** The two fields `updateModel` requires. Every other field left out keeps
 * its stored value, so a write carries only what it changes and never a copy
 * of the rest taken from an earlier read. */
export function modelUpdateKeys(model: ModelRoute) {
  return { upstream_model: model.upstream_model, api_base: model.api_base };
}

/** The model as the gateway has it now, or undefined when it is gone. */
export async function loadModel(id: string): Promise<ModelRoute | undefined> {
  try {
    return await obleth.getModel(id);
  } catch (e) {
    if (e instanceof OblethApiError && e.status === 404) return undefined;
    throw e;
  }
}

// Collects checked tag checkboxes (named `tag_<name>`) from a model form,
// folding in each tag's paired strength level (`tag_level_<name>`, 1-3) into
// a `base:level` string, e.g. { tag_coding: "on", tag_level_coding: "3" } ->
// ["coding:3"]. Level 1 serializes as the bare tag name so an untouched
// model's stored tags round-trip byte-identical; this mirrors the gateway's
// `parse_tag_level` convention on the Rust side.
export function tagsFromForm(formData: FormData): string[] {
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
export function aliasesFromForm(formData: FormData): string[] {
  return namesFromForm(formData, "aliases");
}

// A textarea of names, one per line (commas also accepted), as a list.
export function namesFromForm(formData: FormData, field: string): string[] {
  return String(formData.get(field) ?? "")
    .split(/[\n,]/)
    .map((a) => a.trim())
    .filter((a) => a.length > 0);
}

// Parses the variants field (the settings page's rows, as JSON) into the list
// the gateway stores. Rows without a name are dropped, as blank aliases are;
// the gateway checks the rest (names already taken, unknown boons, the cap),
// so a variant it refuses surfaces as a save error.
export function variantsFromForm(formData: FormData): ModelVariantInput[] {
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
export function upstreamHeadersFromForm(
  formData: FormData,
): { upstream_headers?: Record<string, string | null> } {
  if (!formData.has("upstream_headers")) return {};
  return { upstream_headers: parseUpstreamHeaders(String(formData.get("upstream_headers") ?? "")) };
}

export function clampTagLevel(raw: FormDataEntryValue | null): number {
  const n = Number(raw);
  if (!Number.isFinite(n)) return 0;
  return Math.min(3, Math.max(0, Math.trunc(n)));
}

// Collects checked boon checkboxes (named `boon_<name>`) from a model form into
// an array of boon names, e.g. { boon_vision: "on" } -> ["vision"].
export function boonsFromForm(formData: FormData): string[] {
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
export function toolServersFromForm(formData: FormData): string[] {
  const servers: string[] = [];
  for (const [key, value] of formData.entries()) {
    if (key.startsWith("tool_server_") && value === "on") {
      servers.push(key.slice("tool_server_".length));
    }
  }
  return servers;
}

export function strOrNull(v: FormDataEntryValue | null): string | undefined {
  const s = String(v ?? "").trim();
  return s || undefined;
}

// The session affinity picker and its values as the model's setting: "gateway"
// is `{}`, "off" turns it off, and "on" keeps only the values filled in (the
// rest use the gateway's).
export function sessionAffinityFromForm(formData: FormData): ModelSessionAffinity {
  const mode = String(formData.get("session_affinity_mode") ?? "gateway");
  if (mode === "off") return { enabled: false };
  if (mode !== "on") return {};
  const out: ModelSessionAffinity = { enabled: true };
  const hold = finiteNumber(formData.get("session_affinity_hold_ms"));
  const held = finiteNumber(formData.get("session_affinity_max_held_percent"));
  const bypass = finiteNumber(formData.get("session_affinity_max_bypass_secs"));
  if (hold !== undefined) out.hold_ms = hold;
  if (held !== undefined) out.max_held_percent = held;
  if (bypass !== undefined) out.max_bypass_secs = bypass;
  return out;
}

export function datetimeOrNull(v: FormDataEntryValue | null): string | null {
  const s = String(v ?? "").trim();
  if (!s) return null;
  const date = new Date(s);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}
