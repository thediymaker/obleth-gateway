// @vitest-environment node
import { afterEach, describe, expect, it, vi } from "vitest";
import { memoryAdapter } from "better-auth/adapters/memory";

// A configured OIDC provider, through the REAL better-auth configuration: the
// sign-in must send the IdP the callback its registration names.

const DISCOVERY = "https://idp.example/.well-known/openid-configuration";

afterEach(() => {
  delete process.env.OIDC_PROVIDERS;
  delete process.env.BETTER_AUTH_URL;
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe("OIDC sign-in", { timeout: 30_000 }, () => {
  it("sends the IdP the /api/auth/oauth2/callback/<id> redirect URI", async () => {
    process.env.BETTER_AUTH_URL = "https://dash.example.edu";
    process.env.OIDC_PROVIDERS = JSON.stringify([
      { providerId: "campus", displayName: "Campus", discoveryUrl: DISCOVERY, clientId: "cid", clientSecret: "sec" },
    ]);
    const realFetch = globalThis.fetch;
    vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === DISCOVERY) {
        return new Response(
          JSON.stringify({
            issuer: "https://idp.example",
            authorization_endpoint: "https://idp.example/authorize",
            token_endpoint: "https://idp.example/token",
            userinfo_endpoint: "https://idp.example/userinfo",
            jwks_uri: "https://idp.example/jwks",
          }),
          { headers: { "content-type": "application/json" } },
        );
      }
      return realFetch(input, init);
    });
    const { createAuth } = await import("./better-auth");
    const store = { user: [], session: [], account: [], verification: [] };
    const auth = createAuth(memoryAdapter(store), "x".repeat(32));
    const res = await auth.api.signInSocial({ body: { provider: "campus", callbackURL: "/" } });
    const url = new URL((res as { url: string }).url);
    expect(url.origin + url.pathname).toBe("https://idp.example/authorize");
    expect(url.searchParams.get("redirect_uri")).toBe(
      "https://dash.example.edu/api/auth/oauth2/callback/campus",
    );
  });
});
