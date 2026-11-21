import { cache } from "react";
import { headers } from "next/headers";
import { auth } from "@/lib/auth/better-auth";

export interface SessionUser {
  id: string;
  email: string;
  role: "admin" | "user";
  status: "pending" | "active";
  tenantId: string | null;
}

/**
 * The signed-in user. Memoised per request: a layout and its page both ask,
 * and each ask was a Postgres read.
 */
export const getSession = cache(async (): Promise<SessionUser | null> => {
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
});
