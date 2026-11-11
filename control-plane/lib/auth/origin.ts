/**
 * Whether a request that changes something was sent by another site.
 *
 * The session cookie is `SameSite=Lax`, which keeps it off cross-site POSTs but
 * not same-site ones: a page on another port of this host, or on a sibling
 * subdomain, can POST to the dashboard's API routes with the admin's cookie.
 * Server Actions are covered by Next's own Origin check; route handlers under
 * `/api` are not, so the proxy asks this for every one of them.
 *
 * A browser says where a request came from in `Origin` and, in current
 * browsers, `Sec-Fetch-Site`. A request that carries neither was not sent by a
 * page (curl, a script with the cookie), so it is not a forgery and passes.
 */
export function sentByAnotherSite(method: string, headers: Headers): boolean {
  if (["GET", "HEAD", "OPTIONS"].includes(method.toUpperCase())) return false;
  const origin = headers.get("origin");
  if (origin !== null) {
    // What the browser addressed: the proxy in front, when there is one.
    const host = (headers.get("x-forwarded-host") ?? headers.get("host") ?? "")
      .split(",")[0]
      .trim()
      .toLowerCase();
    let sender: string;
    try {
      sender = new URL(origin).host.toLowerCase();
    } catch {
      // `null` (a sandboxed frame, a redirect across origins) or malformed.
      return true;
    }
    return host === "" || sender !== host;
  }
  const site = headers.get("sec-fetch-site");
  return site !== null && site !== "same-origin" && site !== "none";
}
