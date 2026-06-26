import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => {
  delete process.env.OIDC_PROVIDERS;
  vi.resetModules();
});

describe("oidcProviderLabels", () => {
  it("returns [] when OIDC_PROVIDERS is unset", async () => {
    const { oidcProviderLabels } = await import("./providers");
    expect(oidcProviderLabels()).toEqual([]);
  });

  it("returns only providerId and displayName — no secrets", async () => {
    process.env.OIDC_PROVIDERS = JSON.stringify([{
      providerId: "dex",
      displayName: "Dev SSO (Dex)",
      discoveryUrl: "http://dex:5556/.well-known/openid-configuration",
      clientId: "obleth-gateway",
      clientSecret: "super-secret",
      scopes: ["openid", "email", "profile"],
    }]);
    const { oidcProviderLabels } = await import("./providers");
    const labels = oidcProviderLabels();
    expect(labels).toHaveLength(1);
    expect(labels[0]).toEqual({ providerId: "dex", displayName: "Dev SSO (Dex)" });
    // Security: no secret or sensitive fields must leak
    expect(labels[0]).not.toHaveProperty("clientSecret");
    expect(labels[0]).not.toHaveProperty("clientId");
    expect(labels[0]).not.toHaveProperty("discoveryUrl");
  });

  it("returns multiple labels in order", async () => {
    process.env.OIDC_PROVIDERS = JSON.stringify([
      { providerId: "a", displayName: "Provider A", discoveryUrl: "https://a.example/", clientId: "cid-a", clientSecret: "s1" },
      { providerId: "b", displayName: "Provider B", discoveryUrl: "https://b.example/", clientId: "cid-b", clientSecret: "s2" },
    ]);
    const { oidcProviderLabels } = await import("./providers");
    const labels = oidcProviderLabels();
    expect(labels).toHaveLength(2);
    expect(labels[0].providerId).toBe("a");
    expect(labels[1].providerId).toBe("b");
  });
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
