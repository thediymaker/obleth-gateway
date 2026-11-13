import { describe, expect, it, vi } from "vitest";
import { ensurePersonalTenant, userIdentity, type PersonalDeps } from "./personal";

const ISSUER = "https://bao/v1/identity/oidc/provider/obleth";
const providers = [{ providerId: "openbao", discoveryUrl: `${ISSUER}/.well-known/openid-configuration` }];

function makeDeps(accounts: Array<{ providerId: string; accountId: string }>, over: Partial<PersonalDeps> = {}) {
  const query = vi.fn(async (sql: string) => {
    if (sql.includes('from "account"')) return { rows: accounts };
    if (sql.includes('from "user"')) return { rows: [{ email: "a@x" }] };
    return { rows: [] };
  });
  const provisionIdentity = vi.fn(async () => ({ tenant_id: "t1", tenant_name: "u-s1", key_id: "k", tenant_created: true }));
  const deps: PersonalDeps = { query, provisionIdentity, env: {}, providers, ...over };
  return { deps, query, provisionIdentity };
}
const updates = (q: ReturnType<typeof vi.fn>) => q.mock.calls.filter((c) => String(c[0]).includes("update"));

describe("userIdentity", () => {
  it("maps an OIDC account to issuer and subject", async () => {
    const { deps } = makeDeps([{ providerId: "openbao", accountId: "s1" }]);
    expect(await userIdentity("u1", deps)).toEqual({ issuer: ISSUER, subject: "s1" });
  });
  it("returns null for credential-only users", async () => {
    const { deps } = makeDeps([{ providerId: "credential", accountId: "u1" }]);
    expect(await userIdentity("u1", deps)).toBeNull();
  });
  it("skips providers whose discoveryUrl lacks the exact suffix", async () => {
    const { deps } = makeDeps([{ providerId: "openbao", accountId: "s1" }], {
      providers: [{ providerId: "openbao", discoveryUrl: `${ISSUER}/.well-known/openid-configuration/x` }],
    });
    expect(await userIdentity("u1", deps)).toBeNull();
  });
});

describe("ensurePersonalTenant", () => {
  it("sets tenantId and activates when OIDC_AUTO_APPROVE=true", async () => {
    const { deps, query, provisionIdentity } = makeDeps([{ providerId: "openbao", accountId: "s1" }], {
      env: { OIDC_AUTO_APPROVE: "true" },
    });
    expect(await ensurePersonalTenant("u1", deps)).toBe("t1");
    expect(provisionIdentity).toHaveBeenCalledWith({ issuer: ISSUER, subject: "s1", email: "a@x" });
    const u = updates(query);
    expect(u).toHaveLength(1);
    expect(u[0][1]).toEqual(["u1", "t1", true]);
    expect(String(u[0][0])).not.toContain("role");
  });
  it("leaves status unchanged when the flag is not exactly true", async () => {
    const { deps, query } = makeDeps([{ providerId: "openbao", accountId: "s1" }], { env: { OIDC_AUTO_APPROVE: "1" } });
    await ensurePersonalTenant("u1", deps);
    expect(updates(query)[0][1]).toEqual(["u1", "t1", false]);
  });
  it("does not touch a credential user", async () => {
    const { deps, query, provisionIdentity } = makeDeps([{ providerId: "credential", accountId: "u1" }]);
    expect(await ensurePersonalTenant("u1", deps)).toBeNull();
    expect(updates(query)).toHaveLength(0);
    expect(provisionIdentity).not.toHaveBeenCalled();
  });
  it("never throws when provisioning fails", async () => {
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const { deps, query } = makeDeps([{ providerId: "openbao", accountId: "s1" }], {
      provisionIdentity: vi.fn(async () => {
        throw new Error("boom");
      }),
    });
    expect(await ensurePersonalTenant("u1", deps)).toBeNull();
    expect(updates(query)).toHaveLength(0);
    expect(err).toHaveBeenCalled();
    err.mockRestore();
  });
  it("does not touch a user that has both credential and OIDC accounts", async () => {
    const { deps, query, provisionIdentity } = makeDeps([
      { providerId: "credential", accountId: "u1" },
      { providerId: "openbao", accountId: "s1" },
    ]);
    expect(await ensurePersonalTenant("u1", deps)).toBeNull();
    expect(updates(query)).toHaveLength(0);
    expect(provisionIdentity).not.toHaveBeenCalled();
  });
  it("only activates a user without a tenant yet (no re-approval of demoted users)", async () => {
    const { deps, query } = makeDeps([{ providerId: "openbao", accountId: "s1" }], { env: { OIDC_AUTO_APPROVE: "true" } });
    await ensurePersonalTenant("u1", deps);
    expect(String(updates(query)[0][0])).toContain('"tenantId" is null');
  });
});
