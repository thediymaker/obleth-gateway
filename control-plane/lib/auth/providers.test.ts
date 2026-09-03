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

  it("passes the token-endpoint authentication method through when set", async () => {
    process.env.OIDC_PROVIDERS = JSON.stringify([{
      providerId: "globus",
      displayName: "Globus",
      discoveryUrl: "https://auth.globus.org/.well-known/openid-configuration",
      clientId: "id",
      clientSecret: "secret",
      authentication: "basic",
    }]);
    const { oidcProviders } = await import("./providers");
    expect(oidcProviders()[0].authentication).toBe("basic");
  });

  it("leaves authentication undefined when unset, so better-auth keeps its default", async () => {
    process.env.OIDC_PROVIDERS = JSON.stringify([{
      providerId: "dex",
      displayName: "Dex",
      discoveryUrl: "https://dex.example/.well-known/openid-configuration",
      clientId: "id",
      clientSecret: "secret",
    }]);
    const { oidcProviders } = await import("./providers");
    expect(oidcProviders()[0].authentication).toBeUndefined();
  });

  it("throws on an unrecognised authentication method instead of silently using the body", async () => {
    // "client_secret_basic" is the discovery-document spelling, and an easy
    // thing to copy in by mistake. better-auth would treat it as "not basic"
    // and post the credentials in the body -- the exact bug this field exists
    // to prevent -- so it must not be accepted.
    process.env.OIDC_PROVIDERS = JSON.stringify([{
      providerId: "globus",
      displayName: "Globus",
      discoveryUrl: "https://auth.globus.org/.well-known/openid-configuration",
      clientId: "id",
      clientSecret: "secret",
      authentication: "client_secret_basic",
    }]);
    const { oidcProviders } = await import("./providers");
    expect(() => oidcProviders()).toThrow(/expected one of "basic", "post"/);
  });
});
