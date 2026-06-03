"use server";

import { revalidatePath } from "next/cache";
import { obleth } from "@/lib/obleth";
import { requireSession } from "@/lib/auth/session";

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

export async function createModelAction(formData: FormData) {
  await requireSession();
  await obleth.createModel({
    model_name: String(formData.get("model_name") ?? "").trim(),
    description: String(formData.get("description") ?? "").trim(),
    upstream_model: String(formData.get("upstream_model") ?? "").trim(),
    api_base: String(formData.get("api_base") ?? "").trim(),
    api_key: strOrNull(formData.get("api_key")),
    input_cost_per_token: numOr(formData.get("input_cost_per_token"), 0),
    output_cost_per_token: numOr(formData.get("output_cost_per_token"), 0),
    context_window: numOr(formData.get("context_window"), 8192),
    admission_weight: numOr(formData.get("admission_weight"), 100),
    max_in_flight: numOrNull(formData.get("max_in_flight")),
    supports_function_calling: formData.get("supports_function_calling") === "on",
    supports_system_messages: formData.get("supports_system_messages") === "on",
    supports_response_schema: formData.get("supports_response_schema") === "on",
    supports_tool_choice: formData.get("supports_tool_choice") === "on",
  });
  revalidatePath("/models");
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
    input_cost_per_token: numOr(formData.get("input_cost_per_token"), 0),
    output_cost_per_token: numOr(formData.get("output_cost_per_token"), 0),
    context_window: numOr(formData.get("context_window"), 8192),
    admission_weight: numOr(formData.get("admission_weight"), 100),
    max_in_flight: numOrNull(formData.get("max_in_flight")),
    supports_function_calling: formData.get("supports_function_calling") === "on",
    supports_system_messages: formData.get("supports_system_messages") === "on",
    supports_response_schema: formData.get("supports_response_schema") === "on",
    supports_tool_choice: formData.get("supports_tool_choice") === "on",
    enabled: formData.get("enabled") === "on",
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

export async function createMcpServerAction(formData: FormData) {
  await requireSession();
  const name = String(formData.get("name") ?? "").trim();
  const upstream_url = String(formData.get("upstream_url") ?? "").trim();
  if (!name || !upstream_url) return;
  await obleth.createMcpServer({
    name,
    upstream_url,
    auth_header: strOrNull(formData.get("auth_header")),
  });
  revalidatePath("/mcp");
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
