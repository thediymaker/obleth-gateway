import { getSessionCookie } from "better-auth/cookies";
import { NextResponse, type NextRequest } from "next/server";
import { contentSecurityPolicy, generateNonce } from "@/lib/csp";

export async function proxy(request: NextRequest) {
  const { pathname } = request.nextUrl;

  // A fresh nonce per document. Forwarding the policy on the *request* is what
  // lets Next.js stamp the nonce on the script tags it renders; the same value
  // goes back on the response so the browser enforces it.
  const csp = contentSecurityPolicy(generateNonce());
  const requestHeaders = new Headers(request.headers);
  requestHeaders.set("content-security-policy", csp);
  const pass = () => {
    const res = NextResponse.next({ request: { headers: requestHeaders } });
    res.headers.set("Content-Security-Policy", csp);
    return res;
  };

  if (
    pathname.startsWith("/login") ||
    pathname.startsWith("/awaiting-approval") ||
    pathname.startsWith("/api/auth") ||
    pathname.startsWith("/_next") ||
    pathname.includes(".")
  ) {
    return pass();
  }

  // Presence-only check: getSessionCookie reads the cookie but does NOT validate
  // the session or the caller's role. It exists to redirect anonymous requests to
  // /login for a good UX. Real authorization (active session + admin/user role)
  // is enforced downstream — in the dashboard/portal layouts, server actions, and
  // the /api/live route handlers (see lib/auth/guard.ts). Do not treat passing
  // this middleware as proof the caller is authorized.
  const cookie = getSessionCookie(request);
  if (!cookie) return NextResponse.redirect(new URL("/login", request.url));
  return pass();
}

export const config = {
  // Only build assets are skipped: every HTML document, including 404s for
  // paths like /favicon.ico, must carry the policy.
  matcher: ["/((?!_next/static|_next/image).*)"],
};
