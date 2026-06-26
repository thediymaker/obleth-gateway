"use server";

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { requireAdmin } from "@/lib/auth/roles";
import { assignUser, setUserStatus } from "@/lib/auth/users";
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

export async function assignUserAction(formData: FormData): Promise<ActionResult> {
  await requireAdmin();
  try {
    const p = assignSchema.parse({
      id: formData.get("id"),
      role: formData.get("role"),
      tenantId: formData.get("tenantId") ?? "",
    });
    await assignUser(p.id, p.role, p.tenantId);
    revalidatePath("/users");
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
  }
}

export async function setUserStatusAction(formData: FormData): Promise<ActionResult> {
  await requireAdmin();
  try {
    const id = String(formData.get("id") ?? "").trim();
    if (!id) return { ok: false, error: "Missing user id" };
    const raw = String(formData.get("status") ?? "");
    const status = raw === "active" ? "active" : "pending";
    await setUserStatus(id, status as "active" | "pending");
    revalidatePath("/users");
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
  }
}
