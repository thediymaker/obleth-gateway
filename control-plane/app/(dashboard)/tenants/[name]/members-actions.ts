"use server";

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { requireAdmin } from "@/lib/auth/roles";
import { accounts } from "@/lib/obleth-accounts";
import { resolveIdentityByEmail } from "@/lib/identity-lookup";
import type { ActionResult } from "@/app/actions";

const memberSchema = z.object({
  tenantId: z.string().uuid(),
  email: z.string().trim().toLowerCase().email(),
  role: z.enum(["member", "manager"]),
});
const removeSchema = z.object({
  tenantId: z.string().uuid(),
  issuer: z.string().min(1),
  subject: z.string().min(1),
});

function message(e: unknown): string {
  return e instanceof Error ? e.message : "Request failed";
}

function revalidate() {
  revalidatePath("/tenants/[name]", "page");
  revalidatePath("/users");
}

export async function addTenantMember(formData: FormData): Promise<ActionResult> {
  const admin = await requireAdmin();
  try {
    const p = memberSchema.parse({
      tenantId: formData.get("tenantId"),
      email: formData.get("email"),
      role: formData.get("role") ?? "member",
    });
    const found = await resolveIdentityByEmail(p.email);
    if (!found.ok) return found;
    const ident = found.identity;
    await accounts.upsertMember(
      p.tenantId,
      { issuer: ident.issuer, subject: ident.subject, email: p.email, role: p.role },
      { auditActor: admin.email },
    );
    revalidate();
    return { ok: true };
  } catch (e) {
    return { ok: false, error: message(e) };
  }
}

export async function removeTenantMember(formData: FormData): Promise<ActionResult> {
  const admin = await requireAdmin();
  try {
    const p = removeSchema.parse({
      tenantId: formData.get("tenantId"),
      issuer: formData.get("issuer"),
      subject: formData.get("subject"),
    });
    const members = await accounts.listMembers(p.tenantId);
    if (members.some((m) => m.issuer === p.issuer && m.subject === p.subject && m.role === "owner")) {
      return { ok: false, error: "The owner can't be removed" };
    }
    await accounts.removeMember(p.tenantId, p.issuer, p.subject, { auditActor: admin.email });
    revalidate();
    return { ok: true };
  } catch (e) {
    return { ok: false, error: message(e) };
  }
}
