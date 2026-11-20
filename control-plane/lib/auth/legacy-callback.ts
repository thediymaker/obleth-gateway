// IdP registrations made before better-auth 1.7 name the callback
// /api/auth/oauth2/callback/<id>; 1.7 serves it at /api/auth/callback/<id>.
// lib/auth/providers.ts keeps sending the old URL, and this maps it on.

const LEGACY = "/api/auth/oauth2/callback/";
const CURRENT = "/api/auth/callback/";

/** `req` with a legacy callback path moved to the current one; else `req`. */
export function withCurrentCallbackPath(req: Request): Request {
  const url = new URL(req.url);
  if (!url.pathname.startsWith(LEGACY)) return req;
  url.pathname = CURRENT + url.pathname.slice(LEGACY.length);
  return new Request(url, req);
}
