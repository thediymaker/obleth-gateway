"use server";

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { requireUser } from "@/lib/auth/roles";
import type { SessionUser } from "@/lib/auth/session";
import { obleth } from "@/lib/obleth";

export type PortalResult = { ok: true } | { ok: false; error: string };
export type CreatePortalKeyResult =
  | { ok: true; secret: string }
  | { ok: false; error: string };

/**
 * Verify the key belongs to the caller's tenant before any mutating call.
 * Tenant is always derived from the session — never from client input.
 */
async function assertOwnedKey(tenantId: string, keyId: string): Promise<boolean> {
  const keys = await obleth.listKeys(tenantId);
  return keys.some((k) => k.id === keyId);
}

async function requireTenantUser(): Promise<SessionUser & { tenantId: string }> {
  const user = await requireUser();
  if (!user.tenantId) throw new Error("No tenant assigned to this user");
  return user as SessionUser & { tenantId: string };
}

export async function createPortalKey(
  formData: FormData,
): Promise<CreatePortalKeyResult> {
  const user = await requireTenantUser();
  const rawName = String(formData.get("name") ?? "").trim();
  const parsed = z.string().min(1, "Key name is required").safeParse(rawName);
  if (!parsed.success) {
    return { ok: false, error: parsed.error.issues[0]?.message ?? "Invalid name" };
  }
  try {
    const created = await obleth.createKey(
      user.tenantId,
      { name: parsed.data },
      { auditActor: user.email },
    );
    revalidatePath("/portal/keys");
    return { ok: true, secret: created.secret };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
  }
}

export async function disablePortalKey(
  formData: FormData,
): Promise<PortalResult> {
  const user = await requireTenantUser();
  const id = String(formData.get("id") ?? "");
  if (!id) return { ok: false, error: "Missing key id" };
  if (!(await assertOwnedKey(user.tenantId, id))) {
    return { ok: false, error: "Key not found" };
  }
  const disabled = String(formData.get("disabled")) === "true";
  try {
    await obleth.setKeyDisabled(id, disabled, { auditActor: user.email });
    revalidatePath("/portal/keys");
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
  }
}

export async function deletePortalKey(
  formData: FormData,
): Promise<PortalResult> {
  const user = await requireTenantUser();
  const id = String(formData.get("id") ?? "");
  if (!id) return { ok: false, error: "Missing key id" };
  if (!(await assertOwnedKey(user.tenantId, id))) {
    return { ok: false, error: "Key not found" };
  }
  try {
    await obleth.deleteKey(id, { auditActor: user.email });
    revalidatePath("/portal/keys");
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
  }
}
