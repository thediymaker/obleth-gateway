"use server";

import { redirect } from "next/navigation";
import { revalidatePath } from "next/cache";
import { z } from "zod";
import { requireUser } from "@/lib/auth/roles";
import { accounts } from "@/lib/obleth-accounts";
import { resolveIdentityByEmail } from "@/lib/identity-lookup";
import { currentPortalContext } from "@/lib/portal-context";

export type MembersResult = { ok: true } | { ok: false; error: string };

const NOT_MANAGER = "Only a tenant manager can manage members";
const NEEDS_MANAGER = "A tenant needs a manager";
const OWNER_LOCKED = "The owner cannot be changed from the portal";

const addSchema = z.object({
  email: z.string().trim().toLowerCase().email(),
  role: z.enum(["member", "manager"]),
});
const removeSchema = z.object({ issuer: z.string().min(1), subject: z.string().min(1) });

// The tenant is always ctx.current.id; form data never names a tenant.
async function managerContext() {
  const user = await requireUser();
  const ctx = await currentPortalContext();
  if (ctx.current.kind === "personal" || ctx.current.role !== "manager") return null;
  return { ctx, opts: { auditActor: user.email } };
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : "Request failed";
}

// Form actions must return void; report the outcome back to the members page
// in the query string. Defined here (a "use server" module) so the client
// PortalMembers component can reference them.
function back(r: { ok: true } | { ok: false; error: string }): never {
  const q = new URLSearchParams(r.ok ? { ok: "Saved" } : { error: r.error });
  redirect(`/portal/members?${q}`);
}
export async function addAction(fd: FormData): Promise<void> {
  back(await addMember(fd));
}
export async function removeAction(fd: FormData): Promise<void> {
  back(await removeMember(fd));
}

function wouldLoseLastManager(
  members: { issuer: string; subject: string; role: string }[],
  issuer: string,
  subject: string,
): boolean {
  const target = members.find((m) => m.issuer === issuer && m.subject === subject);
  if (!target || target.role !== "manager") return false;
  return members.filter((m) => m.role === "manager").length <= 1;
}

export async function addMember(formData: FormData): Promise<MembersResult> {
  const m = await managerContext();
  if (!m) return { ok: false, error: NOT_MANAGER };
  try {
    const p = addSchema.parse({ email: formData.get("email"), role: formData.get("role") ?? "member" });
    const found = await resolveIdentityByEmail(p.email);
    if (!found.ok) return found;
    const ident = found.identity;
    const current = await accounts.listMembers(m.ctx.current.id);
    {
      const members = current;
      const existing = members.find((x) => x.issuer === ident.issuer && x.subject === ident.subject);
      if (existing?.role === "owner") return { ok: false, error: OWNER_LOCKED };
      if (p.role !== "manager" && wouldLoseLastManager(members, ident.issuer, ident.subject)) {
        return { ok: false, error: NEEDS_MANAGER };
      }
    }
    await accounts.upsertMember(
      m.ctx.current.id,
      { issuer: ident.issuer, subject: ident.subject, email: p.email, role: p.role },
      m.opts,
    );
    revalidatePath("/portal/members");
    return { ok: true };
  } catch (e) {
    return { ok: false, error: message(e) };
  }
}

export async function removeMember(formData: FormData): Promise<MembersResult> {
  const m = await managerContext();
  if (!m) return { ok: false, error: NOT_MANAGER };
  try {
    const p = removeSchema.parse({ issuer: formData.get("issuer"), subject: formData.get("subject") });
    const members = await accounts.listMembers(m.ctx.current.id);
    if (members.find((x) => x.issuer === p.issuer && x.subject === p.subject)?.role === "owner") {
      return { ok: false, error: OWNER_LOCKED };
    }
    if (wouldLoseLastManager(members, p.issuer, p.subject)) return { ok: false, error: NEEDS_MANAGER };
    await accounts.removeMember(m.ctx.current.id, p.issuer, p.subject, m.opts);
    revalidatePath("/portal/members");
    return { ok: true };
  } catch (e) {
    return { ok: false, error: message(e) };
  }
}
