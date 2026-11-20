"use server";

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { requireAdmin } from "@/lib/auth/roles";
import { assignUser, setUserStatus, type UserChangeResult } from "@/lib/auth/users";
import { actionError, type ActionResult } from "@/lib/action-result";

const assignSchema = z.object({
  id: z.string().min(1),
  role: z.enum(["admin", "user"]),
  tenantId: z
    .string()
    .uuid()
    .or(z.literal(""))
    .transform((v) => (v === "" ? null : v)),
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

const statusSchema = z.object({
  id: z.string().trim().min(1, "Missing user id"),
  status: z.enum(["active", "pending"]),
});

/** Give a user a role and a tenant ("" for none). */
export async function assignUserAction(input: { id: string; role: string; tenantId: string }): Promise<ActionResult> {
  const caller = await requireAdmin();
  try {
    const p = assignSchema.parse(input);
    if (p.role !== "admin" && p.id === caller.id) return { ok: false, error: SELF_REFUSAL };
    return changeResult(await assignUser(p.id, p.role, p.tenantId));
  } catch (e) {
    return actionError(e);
  }
}

/** Approve a user, or send them back to waiting for approval. */
export async function setUserStatusAction(id: string, status: "active" | "pending"): Promise<ActionResult> {
  const caller = await requireAdmin();
  try {
    const p = statusSchema.parse({ id, status });
    if (p.status !== "active" && p.id === caller.id) return { ok: false, error: SELF_REFUSAL };
    return changeResult(await setUserStatus(p.id, p.status));
  } catch (e) {
    return actionError(e);
  }
}
