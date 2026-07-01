import { NextResponse } from "next/server";
import { requireAdmin } from "@/lib/auth/roles";

/**
 * Authorization gate for `/api/live/*` route handlers.
 *
 * The proxy middleware only checks for the *presence* of a session cookie, not
 * the caller's role, so every live REST route must independently confirm the
 * caller is an active admin. Without this, any authenticated user — including a
 * tenant portal user — could reach admin-only data (config backups, other
 * tenants' usage) by calling these endpoints directly.
 *
 * Returns a 401 response to short-circuit the handler when the caller is not an
 * active admin, or `null` to let the handler proceed.
 */
export async function guardAdmin(): Promise<NextResponse | null> {
  try {
    await requireAdmin();
    return null;
  } catch {
    return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
  }
}
