import { getSession, type SessionUser } from "@/lib/auth/session";

export async function requireAdmin(): Promise<SessionUser> {
  const s = await getSession();
  if (!s || s.role !== "admin" || s.status !== "active") throw new Error("Unauthorized");
  return s;
}

export async function requireUser(): Promise<SessionUser> {
  const s = await getSession();
  if (!s || s.status !== "active") throw new Error("Unauthorized");
  return s;
}

export async function requireTenant(): Promise<string> {
  const s = await requireUser();
  if (!s.tenantId) throw new Error("No tenant assigned to this user");
  return s.tenantId;
}
