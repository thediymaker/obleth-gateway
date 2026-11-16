"use server";

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { requireUser } from "@/lib/auth/roles";
import { cookies } from "next/headers";
import { obleth } from "@/lib/obleth";
import { accounts } from "@/lib/obleth-accounts";
import { PORTAL_TENANT_COOKIE, currentPortalContext, loadPortalContext } from "@/lib/portal-context";

export type PortalResult = { ok: true } | { ok: false; error: string };
export type CreatePortalKeyResult =
  | { ok: true; secret: string }
  | { ok: false; error: string };

/**
 * The key must be the caller's own in the selected tenant, or any owned key of
 * that tenant when the caller manages it. Tenant comes from the validated
 * portal context, never from client input.
 */
async function canTouchKey(ctx: Awaited<ReturnType<typeof currentPortalContext>>, keyId: string): Promise<boolean> {
  const own = await accounts.listOwnedKeys(ctx.current.id, ctx.identity);
  if (own.some((k) => k.id === keyId)) return true;
  if (ctx.current.role !== "manager") return false;
  const all = await accounts.listOwnedKeys(ctx.current.id);
  return all.some((k) => k.id === keyId);
}

export async function selectPortalTenant(formData: FormData): Promise<void> {
  const user = await requireUser();
  const wanted = String(formData.get("tenantId") ?? "");
  const ctx = await loadPortalContext(user, wanted);
  if (ctx.tenants.some((t) => t.id === wanted)) {
    (await cookies()).set(PORTAL_TENANT_COOKIE, wanted, {
      httpOnly: true, sameSite: "lax",
      path: "/portal",
      secure: process.env.NODE_ENV === "production",
    });
  }
  // Revalidate the layout segment, not just the page: the shell
  // (TenantSwitcher) lives in the portal layout, and a page-only
  // revalidation leaves it on the stale tenant while the page content
  // already shows the new one (TDDEV-5338 regression).
  revalidatePath("/portal", "layout");
}

export async function createPortalKey(
  formData: FormData,
): Promise<CreatePortalKeyResult> {
  const user = await requireUser();
  const ctx = await currentPortalContext();
  const rawName = String(formData.get("name") ?? "").trim();
  const parsed = z.string().min(1, "Key name is required").max(100, "Key name is too long").safeParse(rawName);
  if (!parsed.success) {
    return { ok: false, error: parsed.error.issues[0]?.message ?? "Invalid name" };
  }
  try {
    const created = await accounts.createOwnedKey(
      ctx.current.id,
      { ...ctx.identity, name: parsed.data },
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
  const user = await requireUser();
  const ctx = await currentPortalContext();
  const id = String(formData.get("id") ?? "");
  if (!id) return { ok: false, error: "Missing key id" };
  if (!(await canTouchKey(ctx, id))) {
    return { ok: false, error: "Key not found" };
  }
  // Disable-only: re-enabling would let a tenant undo an administrator's
  // revocation, so that stays an admin decision.
  if (String(formData.get("disabled")) !== "true") {
    return { ok: false, error: "Contact an administrator to re-enable a key" };
  }
  try {
    await obleth.setKeyDisabled(id, true, { auditActor: user.email });
    revalidatePath("/portal/keys");
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
  }
}

export async function deletePortalKey(
  formData: FormData,
): Promise<PortalResult> {
  const user = await requireUser();
  const ctx = await currentPortalContext();
  const id = String(formData.get("id") ?? "");
  if (!id) return { ok: false, error: "Missing key id" };
  if (!(await canTouchKey(ctx, id))) {
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
