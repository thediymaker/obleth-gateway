import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => {
  delete process.env.OIDC_PROVIDERS;
  vi.resetModules();
});

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

  it("throws when OIDC_PROVIDERS is not valid JSON", async () => {
    process.env.OIDC_PROVIDERS = "not json";
    const { oidcProviders } = await import("./providers");
    expect(() => oidcProviders()).toThrow(/not valid JSON/);
  });

  it("defaults scopes when a provider omits the scopes field", async () => {
    process.env.OIDC_PROVIDERS = JSON.stringify([{
      providerId: "globus",
      displayName: "Globus",
      discoveryUrl: "https://auth.globus.org/.well-known/openid-configuration",
      clientId: "cid",
      clientSecret: "secret",
    }]);
    const { oidcProviders } = await import("./providers");
    const cfgs = oidcProviders();
    expect(cfgs[0].scopes).toEqual(["openid", "email", "profile"]);
  });
});
