"use server";

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { requireUser } from "@/lib/auth/roles";
import type { SessionUser } from "@/lib/auth/session";
import { obleth } from "@/lib/obleth";
import { actionError, type ActionResult } from "@/lib/action-result";

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

export async function createPortalKey(name: string): Promise<CreatePortalKeyResult> {
  const user = await requireTenantUser();
  const parsed = z.string().trim().min(1, "Key name is required").safeParse(name);
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
    return actionError(e);
  }
}

/**
 * Turn one of the tenant's keys off. There is no portal action to turn one
 * back on: that would let a tenant undo an administrator's revocation.
 */
export async function disablePortalKey(id: string): Promise<ActionResult> {
  const user = await requireTenantUser();
  if (!id) return { ok: false, error: "Missing key id" };
  if (!(await assertOwnedKey(user.tenantId, id))) {
    return { ok: false, error: "Key not found" };
  }
  try {
    await obleth.setKeyDisabled(id, true, { auditActor: user.email });
    revalidatePath("/portal/keys");
    return { ok: true };
  } catch (e) {
    return actionError(e);
  }
}

export async function deletePortalKey(id: string): Promise<ActionResult> {
  const user = await requireTenantUser();
  if (!id) return { ok: false, error: "Missing key id" };
  if (!(await assertOwnedKey(user.tenantId, id))) {
    return { ok: false, error: "Key not found" };
  }
  try {
    await obleth.deleteKey(id, { auditActor: user.email });
    revalidatePath("/portal/keys");
    return { ok: true };
  } catch (e) {
    return actionError(e);
  }
}
