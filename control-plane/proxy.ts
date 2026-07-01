import { getSessionCookie } from "better-auth/cookies";
import { NextResponse, type NextRequest } from "next/server";

export async function proxy(request: NextRequest) {
  const { pathname } = request.nextUrl;

  if (
    pathname.startsWith("/login") ||
    pathname.startsWith("/awaiting-approval") ||
    pathname.startsWith("/api/auth") ||
    pathname.startsWith("/_next") ||
    pathname.includes(".")
  ) {
    return NextResponse.next();
  }

  // Presence-only check: getSessionCookie reads the cookie but does NOT validate
  // the session or the caller's role. It exists to redirect anonymous requests to
  // /login for a good UX. Real authorization (active session + admin/user role)
  // is enforced downstream — in the dashboard/portal layouts, server actions, and
  // the /api/live route handlers (see lib/auth/guard.ts). Do not treat passing
  // this middleware as proof the caller is authorized.
  const cookie = getSessionCookie(request);
  if (!cookie) return NextResponse.redirect(new URL("/login", request.url));
  return NextResponse.next();
}

export const config = {
  matcher: ["/((?!_next/static|_next/image|favicon.ico).*)"],
};
