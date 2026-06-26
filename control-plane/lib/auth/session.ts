import { headers } from "next/headers";
import { auth } from "@/lib/auth/better-auth";

export interface SessionUser {
  id: string;
  email: string;
  role: "admin" | "user";
  status: "pending" | "active";
  tenantId: string | null;
}

export async function getSession(): Promise<SessionUser | null> {
  const res = await auth.api.getSession({ headers: await headers() });
  if (!res?.user) return null;
  const u = res.user as Record<string, unknown>;
  return {
    id: String(u.id),
    email: String(u.email),
    role: (u.role as "admin" | "user") ?? "user",
    status: (u.status as "pending" | "active") ?? "pending",
    tenantId: (u.tenantId as string | null) ?? null,
  };
}

/**
 * Authorize the current request. Server Actions are POST routes that the Next.js
 * proxy does not reliably cover, so every privileged action must call this to
 * fail closed when the caller is unauthenticated.
 */
export async function requireSession(): Promise<SessionUser> {
  const s = await getSession();
  if (!s) throw new Error("Unauthorized");
  return s;
}
