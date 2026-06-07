"use server";

import { revalidatePath } from "next/cache";
import { parse as parseYaml } from "yaml";
import { obleth, OblethApiError } from "@/lib/obleth";
import type { ModelRoute, UpdateAlertSettings, UpdateAutoRouterSettings } from "@/lib/obleth";
import { requireSession } from "@/lib/auth/session";

export type ActionResult = { ok: true } | { ok: false; error: string };

function actionError(e: unknown): ActionResult {
  if (e instanceof OblethApiError) return { ok: false, error: e.message };
  return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
}

export async function createTenantAction(formData: FormData) {
  await requireSession();
  const name = String(formData.get("name") ?? "").trim();
  if (!name) return;
  await obleth.createTenant({
    name,
    weight: numOrUndef(formData.get("weight")),
    tokens_per_minute: numOrUndef(formData.get("tokens_per_minute")),
    max_in_flight: numOrUndef(formData.get("max_in_flight")),
  });
  revalidatePath("/tenants");
  revalidatePath("/");
}

export async function updateTenantAction(formData: FormData) {
  await requireSession();
  const id = String(formData.get("id") ?? "").trim();
  const name = String(formData.get("name") ?? "").trim();
  if (!id || !name) return;
  await obleth.updateTenant(id, {
    name,
    description: String(formData.get("description") ?? "").trim(),
    organization: String(formData.get("organization") ?? "").trim(),
    contact_email: String(formData.get("contact_email") ?? "").trim(),
  });
  revalidatePath("/tenants");
  revalidatePath("/");
}

export async function setTenantStatusAction(id: string, status: string) {
  await requireSession();
  if (!id) return;
  await obleth.setTenantStatus(id, status);
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
    weekly_windows?: { day: number; start_min: number; end_min: number }[] | null;
  },
): Promise<ActionResult> {
  await requireSession();
  if (!id) return { ok: false, error: "Missing tenant id" };
  try {
    await obleth.setTenantSchedule(id, body);
  } catch (e) {
    return actionError(e);
  }
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
  revalidatePath("/tenants");
  revalidatePath("/");
  return { ok: true };
}

export async function deleteTenantAction(id: string) {
  await requireSession();
  if (!id) return;
  await obleth.deleteTenant(id);
  revalidatePath("/tenants");
  revalidatePath("/keys");
  revalidatePath("/fairshare");
  revalidatePath("/");
}

export async function setWeightAction(id: string, weight: number) {
  await requireSession();
  await obleth.setWeight(id, weight);
  revalidatePath("/tenants");
  revalidatePath("/fairshare");
  revalidatePath("/");
}

export async function setQuotaAction(formData: FormData) {
  await requireSession();
  const id = String(formData.get("id"));
  const tpm = numOrUndef(formData.get("tokens_per_minute"));
  const mif = numOrNull(formData.get("max_in_flight"));
  if (!id || !tpm || tpm <= 0 || (mif !== null && mif <= 0)) return;
  await obleth.setQuota(id, tpm, mif);
  revalidatePath("/tenants");
  revalidatePath("/");
}

export async function createKeyAction(formData: FormData): Promise<string | null> {
  await requireSession();
  const tenantId = String(formData.get("tenant_id"));
  const name = String(formData.get("name") ?? "key").trim() || "key";
  const created = await obleth.createKey(tenantId, name);
  revalidatePath("/keys");
  return created.secret;
}

export async function toggleKeyAction(id: string, disabled: boolean) {
  await requireSession();
  await obleth.setKeyDisabled(id, disabled);
  revalidatePath("/keys");
}

export async function deleteKeyAction(id: string) {
  await requireSession();
  await obleth.deleteKey(id);
  revalidatePath("/keys");
  revalidatePath("/");
}

export async function deleteKeysAction(ids: string[]): Promise<{ deleted: number; failed: number }> {
  await requireSession();
  const uniqueIds = [...new Set(ids.map((id) => String(id)).filter(Boolean))];
  const result = await deleteKeys(uniqueIds);
  revalidatePath("/keys");
  revalidatePath("/");
  return result;
}

export async function deleteFilteredKeysAction(filters: {
  query?: string;
  tenantId?: string;
  status?: "all" | "active" | "disabled";
}): Promise<{ deleted: number; failed: number; matched: number }> {
  await requireSession();
  const query = String(filters.query ?? "").trim().toLowerCase();
  const tenantId = String(filters.tenantId ?? "all");
  const status = filters.status ?? "all";
  const hasFilter = query !== "" || tenantId !== "all" || status !== "all";
  if (!hasFilter) return { deleted: 0, failed: 0, matched: 0 };

  const [tenants, keys] = await Promise.all([obleth.listTenants(), obleth.listKeys()]);
  const tenantNames = new Map(tenants.map((tenant) => [tenant.id, tenant.name]));
  const matched = keys.filter((key) => {
    if (tenantId !== "all" && key.tenant_id !== tenantId) return false;
    if (status === "active" && key.disabled) return false;
    if (status === "disabled" && !key.disabled) return false;
    if (!query) return true;
    const tenantName = tenantNames.get(key.tenant_id) ?? key.tenant_id.slice(0, 8);
    return (
      key.key_prefix.toLowerCase().includes(query) ||
      key.name.toLowerCase().includes(query) ||
      tenantName.toLowerCase().includes(query)
    );
  });

  const result = await deleteKeys(matched.map((key) => key.id));
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

export async function createModelAction(formData: FormData): Promise<ActionResult> {
  await requireSession();
  try {
    await obleth.createModel({
      model_name: String(formData.get("model_name") ?? "").trim(),
      description: String(formData.get("description") ?? "").trim(),
      upstream_model: String(formData.get("upstream_model") ?? "").trim(),
      api_base: String(formData.get("api_base") ?? "").trim(),
      api_key: strOrNull(formData.get("api_key")),
      model_type: String(formData.get("model_type") ?? "chat").trim() || "chat",
      input_cost_per_token: numOr(formData.get("input_cost_per_token"), 0),
      output_cost_per_token: numOr(formData.get("output_cost_per_token"), 0),
      cost_per_image: numOr(formData.get("cost_per_image"), 0),
      cost_per_audio_second: numOr(formData.get("cost_per_audio_second"), 0),
      cost_per_character: numOr(formData.get("cost_per_character"), 0),
      context_window: numOr(formData.get("context_window"), 8192),
      admission_weight: numOr(formData.get("admission_weight"), 100),
      max_in_flight: numOrNull(formData.get("max_in_flight")),
      supports_function_calling: formData.get("supports_function_calling") === "on",
      supports_system_messages: formData.get("supports_system_messages") === "on",
      supports_response_schema: formData.get("supports_response_schema") === "on",
      supports_tool_choice: formData.get("supports_tool_choice") === "on",
      tags: tagsFromForm(formData),
    });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/models");
  return { ok: true };
}

export async function setModelCapacityAction(id: string, max_in_flight: number | null) {
  await requireSession();
  await obleth.setModelCapacity(id, max_in_flight);
  revalidatePath("/models");
  revalidatePath("/fairshare");
}

export async function setModelWeightAction(id: string, admission_weight: number) {
  await requireSession();
  await obleth.setModelWeight(id, admission_weight);
  revalidatePath("/models");
  revalidatePath("/fairshare");
}

export async function deleteModelAction(id: string) {
  await requireSession();
  await obleth.deleteModel(id);
  revalidatePath("/models");
}

export async function setModelCacheAction(id: string, enabled: boolean, ttlSecs?: number) {
  await requireSession();
  await obleth.setModelCache(id, enabled, ttlSecs);
  revalidatePath("/models");
}

export async function updateModelAction(formData: FormData) {
  await requireSession();
  const id = String(formData.get("id") ?? "");
  if (!id) return;
  await obleth.updateModel(id, {
    description: String(formData.get("description") ?? "").trim(),
    upstream_model: String(formData.get("upstream_model") ?? "").trim(),
    api_base: String(formData.get("api_base") ?? "").trim(),
    api_key: strOrNull(formData.get("api_key")),
    model_type: String(formData.get("model_type") ?? "chat").trim() || "chat",
    input_cost_per_token: numOr(formData.get("input_cost_per_token"), 0),
    output_cost_per_token: numOr(formData.get("output_cost_per_token"), 0),
    cost_per_image: numOr(formData.get("cost_per_image"), 0),
    cost_per_audio_second: numOr(formData.get("cost_per_audio_second"), 0),
    cost_per_character: numOr(formData.get("cost_per_character"), 0),
    context_window: numOr(formData.get("context_window"), 8192),
    admission_weight: numOr(formData.get("admission_weight"), 100),
    max_in_flight: numOrNull(formData.get("max_in_flight")),
    supports_function_calling: formData.get("supports_function_calling") === "on",
    supports_system_messages: formData.get("supports_system_messages") === "on",
    supports_response_schema: formData.get("supports_response_schema") === "on",
    supports_tool_choice: formData.get("supports_tool_choice") === "on",
    enabled: formData.get("enabled") === "on",
    tags: tagsFromForm(formData),
  });
  revalidatePath("/models");
  revalidatePath("/fairshare");
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
  | { ok: true; created: number; updated: number; failed: number; errors: string[] }
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
export async function planModelImportAction(text: string): Promise<ImportPlanResult> {
  await requireSession();
  const read = readModelInputs(text);
  if (read.error) return { ok: false, error: read.error };

  let existing: ModelRoute[];
  try {
    existing = await obleth.listModels();
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Failed to load existing models." };
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
export async function importModelsAction(text: string): Promise<ImportModelsResult> {
  await requireSession();
  const read = readModelInputs(text);
  if (read.error) return { ok: false, error: read.error };
  const inputs = read.inputs;

  let existing: ModelRoute[];
  try {
    existing = await obleth.listModels();
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Failed to load existing models." };
  }
  const byName = new Map(existing.map((m) => [m.model_name, m]));

  let created = 0;
  let updated = 0;
  const errors: string[] = [];

  for (const input of inputs) {
    try {
      const found = byName.get(input.model_name);
      if (found) {
        await obleth.updateModel(found.id, {
          description: input.description ?? found.description,
          upstream_model: input.upstream_model,
          api_base: input.api_base,
          api_key: input.api_key ?? undefined,
          model_type: input.model_type ?? found.model_type,
          input_cost_per_token: input.input_cost_per_token ?? found.input_cost_per_token,
          output_cost_per_token: input.output_cost_per_token ?? found.output_cost_per_token,
          cost_per_image: input.cost_per_image ?? found.cost_per_image,
          cost_per_audio_second: input.cost_per_audio_second ?? found.cost_per_audio_second,
          cost_per_character: input.cost_per_character ?? found.cost_per_character,
          context_window: input.context_window ?? found.context_window,
          admission_weight: input.admission_weight ?? found.admission_weight,
          max_in_flight: input.max_in_flight !== undefined ? input.max_in_flight : found.max_in_flight,
          supports_function_calling: input.supports_function_calling ?? found.supports_function_calling,
          supports_system_messages: input.supports_system_messages ?? found.supports_system_messages,
          supports_response_schema: input.supports_response_schema ?? found.supports_response_schema,
          supports_tool_choice: input.supports_tool_choice ?? found.supports_tool_choice,
          enabled: input.enabled ?? found.enabled,
          tags: input.tags ?? found.tags,
        });
        updated += 1;
      } else {
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
          enabled: input.enabled ?? true,
          tags: input.tags ?? [],
        });
        created += 1;
      }
    } catch (e) {
      const detail = e instanceof OblethApiError ? e.message : e instanceof Error ? e.message : "unknown error";
      errors.push(`${input.model_name}: ${detail}`);
    }
  }

  revalidatePath("/models");
  revalidatePath("/fairshare");
  return { ok: true, created, updated, failed: errors.length, errors };
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

export async function createMcpServerAction(formData: FormData): Promise<ActionResult> {
  await requireSession();
  const name = String(formData.get("name") ?? "").trim();
  const upstream_url = String(formData.get("upstream_url") ?? "").trim();
  if (!name || !upstream_url) {
    return { ok: false, error: "Name and upstream URL are required." };
  }
  try {
    await obleth.createMcpServer({
      name,
      upstream_url,
      auth_header: strOrNull(formData.get("auth_header")),
    });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/mcp");
  return { ok: true };
}

export async function toggleMcpServerAction(id: string, upstreamUrl: string, enabled: boolean) {
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

export async function setUsageRetentionAction(days: number): Promise<ActionResult> {
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
  ActionResult & { results?: { channel: string; ok: boolean; detail: string }[] }
> {
  await requireSession();
  try {
    const res = await obleth.testAlert();
    return { ok: true, results: res.results };
  } catch (e) {
    return actionError(e);
  }
}

async function deleteKeys(ids: string[]): Promise<{ deleted: number; failed: number }> {
  let deleted = 0;
  let failed = 0;
  const chunkSize = 25;
  for (let i = 0; i < ids.length; i += chunkSize) {
    const chunk = ids.slice(i, i + chunkSize);
    const results = await Promise.allSettled(chunk.map((id) => obleth.deleteKey(id)));
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
  enabled?: boolean;
  tags?: string[];
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

// Parses and normalizes an uploaded obleth models template into import inputs.
// Returns a human-readable `error` instead of throwing so callers can surface
// it directly. The template is YAML or JSON with a top-level `models:` list.
function readModelInputs(text: string): { inputs: ModelImportInput[]; error?: string } {
  if (!text || !text.trim()) {
    return { inputs: [], error: "No file content provided." };
  }

  let parsed: unknown;
  try {
    parsed = parseModelDocument(text);
  } catch {
    return { inputs: [], error: "Could not parse file. Expected an obleth models YAML/JSON template." };
  }

  const inputs = extractModelEntries(parsed)
    .map(toImportInput)
    .filter((m): m is ModelImportInput => m != null);

  if (inputs.length === 0) {
    return {
      inputs: [],
      error: "No valid models found. Use the obleth template: a top-level `models:` list where each entry has model_name, upstream_model and api_base.",
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
    max_in_flight: entry.max_in_flight == null ? undefined : coerceNum(entry.max_in_flight) ?? null,
    supports_function_calling: coerceBool(entry.supports_function_calling),
    supports_system_messages: coerceBool(entry.supports_system_messages),
    supports_response_schema: coerceBool(entry.supports_response_schema),
    supports_tool_choice: coerceBool(entry.supports_tool_choice),
    enabled: coerceBool(entry.enabled),
    tags: Array.isArray(entry.tags) ? entry.tags.map(coerceStr).filter(Boolean) : undefined,
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
