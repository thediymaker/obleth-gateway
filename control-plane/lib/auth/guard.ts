import { NextResponse, type NextRequest } from "next/server";
import { requireAdmin } from "@/lib/auth/roles";
import type { SessionUser } from "@/lib/auth/session";
import { OblethApiError } from "@/lib/obleth";

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

/**
 * `guardAdmin()` for mutating routes that must attribute the change: also
 * returns the admin's session so the handler can pass `auditActor` to the
 * Management API. A union, so `if (denied) return denied;` narrows `session`.
 */
export async function guardAdminSession(): Promise<
  { denied: NextResponse; session: null } | { denied: null; session: SessionUser }
> {
  try {
    return { denied: null, session: await requireAdmin() };
  } catch {
    return { denied: NextResponse.json({ error: "Unauthorized" }, { status: 401 }), session: null };
  }
}

/** A failure a route means to report: its status and message reach the caller as they are. */
export class RouteError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "RouteError";
  }
}

/** The request's JSON body. A body that isn't JSON is the caller's mistake, so it is a 400. */
export async function jsonBody<T = unknown>(req: Request): Promise<T> {
  try {
    return (await req.json()) as T;
  } catch {
    throw new RouteError(400, "The request body is not valid JSON.");
  }
}

/**
 * The response for a failed `/api/live` handler. The gateway's own status and
 * message pass through (a 404 stays a 404); 502 means the gateway could not be
 * reached and 504 that it did not answer in time. Anything else is a fault in
 * the dashboard: it is logged here and the caller gets a 500 without details.
 */
export function liveError(e: unknown): NextResponse {
  if (e instanceof RouteError || e instanceof OblethApiError) {
    return NextResponse.json({ error: e.message }, { status: e.status });
  }
  // A timeout or an abort is a DOMException, which is not always an Error.
  const name = typeof e === "object" && e !== null && "name" in e ? e.name : undefined;
  if (name === "TimeoutError") {
    return NextResponse.json({ error: "The gateway did not answer in time." }, { status: 504 });
  }
  if (e instanceof TypeError && e.message === "fetch failed") {
    return NextResponse.json({ error: "The gateway could not be reached." }, { status: 502 });
  }
  // The caller went away (Stop, or leaving the page): nobody reads this.
  if (name === "AbortError") return NextResponse.json({ error: "Cancelled." }, { status: 499 });
  console.error("/api/live handler failed", e);
  return NextResponse.json({ error: "Unexpected error." }, { status: 500 });
}

/**
 * An `/api/live` route handler: every one of them is for active admins only.
 * The handler gets the route's params and the admin's session (for
 * `auditActor`), and returns either data, sent as JSON, or a Response of its
 * own (a stream, a file, a status it chose). Errors become `liveError()`.
 */
export function liveRoute<P = Record<string, never>>(
  handler: (req: NextRequest, ctx: { params: P; session: SessionUser }) => Promise<unknown>,
) {
  return async (req: NextRequest, ctx?: { params: Promise<P> }): Promise<Response> => {
    let session: SessionUser;
    try {
      session = await requireAdmin();
    } catch {
      return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
    }
    try {
      const out = await handler(req, { params: (await ctx?.params) as P, session });
      return out instanceof Response ? out : NextResponse.json(out ?? null);
    } catch (e) {
      return liveError(e);
    }
  };
}
