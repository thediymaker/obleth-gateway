import { describe, expect, it, vi } from "vitest";
import { resolveIdentityByEmail, type IdentityLookupDeps } from "./identity-lookup";

function deps(o: Partial<IdentityLookupDeps> = {}): IdentityLookupDeps {
  return {
    identitiesByEmail: vi.fn(async () => []),
    dashboardUserId: vi.fn(async () => null),
    userIdentity: vi.fn(async () => null),
    ensurePersonalTenant: vi.fn(async () => "t1"),
    ...o,
  };
}

describe("resolveIdentityByEmail", () => {
  it("returns the single gateway identity without touching the dashboard", async () => {
    const d = deps({ identitiesByEmail: vi.fn(async () => [{ issuer: "i", subject: "s" }]) });
    expect(await resolveIdentityByEmail("a@x.io", d)).toEqual({ ok: true, identity: { issuer: "i", subject: "s", email: "a@x.io" } });
    expect(d.dashboardUserId).not.toHaveBeenCalled();
  });

  it("rejects two matches", async () => {
    const d = deps({ identitiesByEmail: vi.fn(async () => [{ issuer: "i", subject: "s" }, { issuer: "i", subject: "t" }]) });
    expect(await resolveIdentityByEmail("a@x.io", d)).toEqual({ ok: false, error: "Email matches multiple identities" });
  });

  it("provisions a dashboard OIDC user that has no identity yet", async () => {
    const d = deps({
      dashboardUserId: vi.fn(async () => "u1"),
      userIdentity: vi.fn(async () => ({ issuer: "i", subject: "s" })),
    });
    expect(await resolveIdentityByEmail("a@x.io", d)).toEqual({ ok: true, identity: { issuer: "i", subject: "s", email: "a@x.io" } });
    expect(d.ensurePersonalTenant).toHaveBeenCalledWith("u1");
  });

  it("errors for a credential user or an unknown email", async () => {
    const cred = deps({ dashboardUserId: vi.fn(async () => "u1") });
    expect(await resolveIdentityByEmail("a@x.io", cred)).toEqual({ ok: false, error: "User must sign in once first" });
    expect(cred.ensurePersonalTenant).not.toHaveBeenCalled();
    expect(await resolveIdentityByEmail("a@x.io", deps())).toEqual({ ok: false, error: "User must sign in once first" });
  });

  it("errors when provisioning fails", async () => {
    const d = deps({
      dashboardUserId: vi.fn(async () => "u1"),
      userIdentity: vi.fn(async () => ({ issuer: "i", subject: "s" })),
      ensurePersonalTenant: vi.fn(async () => null),
    });
    expect((await resolveIdentityByEmail("a@x.io", d)).ok).toBe(false);
  });
});
