import { afterEach, describe, expect, it } from "vitest";

afterEach(() => { delete process.env.OIDC_PROVIDERS; });

describe("oidcProviders", () => {
  it("returns [] when OIDC_PROVIDERS is unset", async () => {
    const { oidcProviders } = await import("./providers");
    expect(oidcProviders()).toEqual([]);
  });

  it("maps a configured provider to a genericOAuth config", async () => {
    process.env.OIDC_PROVIDERS = JSON.stringify([{
      providerId: "globus",
      displayName: "Globus",
      discoveryUrl: "https://auth.globus.org/.well-known/openid-configuration",
      clientId: "cid",
      clientSecret: "secret",
      scopes: ["openid", "email", "profile"],
    }]);
    const { oidcProviders } = await import("./providers");
    const cfgs = oidcProviders();
    expect(cfgs).toHaveLength(1);
    expect(cfgs[0]).toMatchObject({
      providerId: "globus",
      discoveryUrl: "https://auth.globus.org/.well-known/openid-configuration",
      clientId: "cid",
      clientSecret: "secret",
    });
  });
});
