"use server";

/** MCP servers. */

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { obleth } from "@/lib/obleth";
import { requireAdmin } from "@/lib/auth/roles";
import { actionError, type ActionResult } from "@/lib/action-result";
import { deleteAndRevalidate, firstIssue, loadModel, MCP_NAME_HINT, MCP_NAME_RE, mcpCreateSchema, modelUpdateKeys, strOrNull } from "./shared";

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

/** Let a tenant's keys call an MCP server directly at `/mcp/<name>`, or stop them. A server no tenant is named for takes no direct calls. */
export async function setMcpServerTenantAction(id: string, tenantId: string, on: boolean): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    const server = (await obleth.listMcpServers()).find((m) => m.id === id);
    if (!server) return { ok: false, error: "That server no longer exists." };
    const current = server.allowed_tenants ?? [];
    const next = on ? [...new Set([...current, tenantId])] : current.filter((t) => t !== tenantId);
    await obleth.updateMcpServer(id, { upstream_url: server.upstream_url, allowed_tenants: next }, { auditActor: session.email });
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
    await obleth.updateModel(modelId, { ...modelUpdateKeys(model), tool_servers: next }, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
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
