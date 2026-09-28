"use server";

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { requireAdmin } from "@/lib/auth/roles";
import { assignUser, setUserStatus, type UserChangeResult } from "@/lib/auth/users";
import type { ActionResult } from "@/app/actions";

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

export async function assignUserAction(formData: FormData): Promise<ActionResult> {
  const caller = await requireAdmin();
  try {
    const p = assignSchema.parse({
      id: formData.get("id"),
      role: formData.get("role"),
      tenantId: formData.get("tenantId") ?? "",
    });
    if (p.role !== "admin" && p.id === caller.id) return { ok: false, error: SELF_REFUSAL };
    return changeResult(await assignUser(p.id, p.role, p.tenantId));
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
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
