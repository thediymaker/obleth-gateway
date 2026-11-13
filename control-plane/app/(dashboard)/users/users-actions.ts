"use server";

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { requireAdmin } from "@/lib/auth/roles";
import { userIdentity } from "@/lib/auth/personal";
import { assignUser, setUserStatus, type UserChangeResult } from "@/lib/auth/users";
import { getDb } from "@/lib/db";
import { accounts } from "@/lib/obleth-accounts";
import type { ActionResult } from "@/app/actions";

const assignSchema = z.object({
  id: z.string().min(1),
  role: z.enum(["admin", "user"]),
  tenantIds: z.array(z.string().uuid()),
  knownTenantIds: z.array(z.string().uuid()),
  syncTenants: z.boolean(),
});

// Stripping admin access from yourself, or from the last active admin (checked
// atomically in lib/auth/users.ts), would lock every administrator out.
const SELF_REFUSAL = "You cannot remove admin access from your own account";

function changeResult(result: UserChangeResult): ActionResult {
  if (result === "last-admin") return { ok: false, error: "Cannot remove the last active admin" };
  if (result === "not-found") return { ok: false, error: "User not found" };
  revalidatePath("/users");
  return { ok: true };
}

/**
 * Role, approval, and (when syncTenants) the shared tenants the user belongs to.
 * New tenants join as member; existing memberships keep their role. Only a
 * tenant the admin saw (knownTenantIds) and unchecked is removed, so one added
 * since the page loaded stays; an owner never is. Personal tenants are not touched.
 */
export async function assignUserAction(formData: FormData): Promise<ActionResult> {
  const caller = await requireAdmin();
  try {
    const p = assignSchema.parse({
      id: formData.get("id"),
      role: formData.get("role"),
      tenantIds: formData.getAll("tenantIds").map(String).filter(Boolean),
      knownTenantIds: formData.getAll("knownTenantIds").map(String).filter(Boolean),
      syncTenants: formData.get("syncTenants") === "1",
    });
    if (p.role !== "admin" && p.id === caller.id) return { ok: false, error: SELF_REFUSAL };
    const res = changeResult(await assignUser(p.id, p.role));
    if (!res.ok || !p.syncTenants) return res;
    try {
      await syncTenants(p.id, new Set(p.tenantIds), new Set(p.knownTenantIds), caller.email);
    } catch (e) {
      return { ok: false, error: `Role saved, tenants not: ${e instanceof Error ? e.message : "request failed"}` };
    }
    revalidatePath("/tenants/[name]", "page");
    return res;
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
  }
}

async function syncTenants(userId: string, wanted: Set<string>, known: Set<string>, actor: string) {
  const identity = await userIdentity(userId);
  if (!identity) {
    if (wanted.size) throw new Error("a password account can't be a tenant member");
    return;
  }
  const { rows } = await getDb().query('select email from "user" where id = $1', [userId]);
  const email = rows[0]?.email as string | undefined;
  const current = (await accounts.memberships(identity.issuer, identity.subject)).filter((m) => m.kind !== "personal");
  const have = new Set(current.map((m) => m.tenant_id));
  for (const tenantId of wanted) {
    if (!have.has(tenantId)) {
      await accounts.upsertMember(tenantId, { ...identity, email, role: "member" }, { auditActor: actor });
    }
  }
  for (const m of current) {
    if (known.has(m.tenant_id) && !wanted.has(m.tenant_id) && m.role !== "owner") {
      await accounts.removeMember(m.tenant_id, identity.issuer, identity.subject, { auditActor: actor });
    }
  }
}

export async function setUserStatusAction(formData: FormData): Promise<ActionResult> {
  const caller = await requireAdmin();
  try {
    const id = String(formData.get("id") ?? "").trim();
    if (!id) return { ok: false, error: "Missing user id" };
    const raw = String(formData.get("status") ?? "");
    const status = raw === "active" ? "active" : "pending";
    if (status !== "active" && id === caller.id) return { ok: false, error: SELF_REFUSAL };
    return changeResult(await setUserStatus(id, status));
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
  }
}
